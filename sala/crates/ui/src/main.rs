//! `sala-ui`: Tauri shell with no business logic (D-010). It forwards JSON lines
//! between the core's named pipe and the web frontend and nothing else.
//!
//! - Frontend -> core: command `send(msg)` (validated as `UiToCore`, written as one line).
//! - Core -> frontend: event `core` with the raw JSON; `CoreToUi::Shutdown` exits at once.
//! - Link state: event `link` (`"connected"` / `"disconnected"`), command `link_state`.

#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sala_protocol::ipc_client::{IpcClient, IpcReader};
use sala_protocol::{UiToCore, current_user_pipe_name, encode_ipc_line};
use serde_json::Value;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::mpsc;

const RECONNECT_DELAY: Duration = Duration::from_secs(1);
/// How long to wait for a core launched by us to open its pipe (spec §3).
const CORE_START_TIMEOUT: Duration = Duration::from_secs(8);
const CORE_START_POLL: Duration = Duration::from_millis(200);

/// Outgoing lines of the current connection; `None` while disconnected.
type Link = Arc<Mutex<Option<mpsc::UnboundedSender<String>>>>;

/// Value of `--pipe <name>`, if present and non-empty.
fn pipe_arg(args: &[String]) -> Option<String> {
    let i = args.iter().position(|a| a == "--pipe")?;
    args.get(i + 1).filter(|v| !v.is_empty()).cloned()
}

/// Explicit `--pipe`, else the pipe of the current user without profile.
fn resolve_pipe(args: &[String], default: impl FnOnce() -> String) -> (String, bool) {
    match pipe_arg(args) {
        Some(p) => (p, true),
        None => (default(), false),
    }
}

/// `concord.exe` in the same folder as `exe`.
fn core_exe_next_to(exe: &Path) -> PathBuf {
    exe.parent().map(Path::to_path_buf).unwrap_or_default().join("concord.exe")
}

/// Checks that `msg` is a known command and turns it into a line.
fn command_line(msg: Value) -> Result<String, String> {
    let cmd: UiToCore = serde_json::from_value(msg).map_err(|e| format!("invalid command: {e}"))?;
    encode_ipc_line(&cmd).map_err(|e| e.to_string())
}

fn is_shutdown(msg: &Value) -> bool {
    msg.get("type").and_then(Value::as_str) == Some("Shutdown")
}

#[tauri::command]
fn send(msg: Value, link: State<'_, Link>) -> Result<(), String> {
    let line = command_line(msg)?;
    match link.lock().unwrap().as_ref() {
        Some(tx) => tx.send(line).map_err(|_| "no connection to the core".to_string()),
        None => Err("no connection to the core".to_string()),
    }
}

#[tauri::command]
fn link_state(link: State<'_, Link>) -> &'static str {
    if link.lock().unwrap().is_some() { "connected" } else { "disconnected" }
}

fn launch_core() {
    use std::os::windows::process::CommandExt;
    let Ok(exe) = std::env::current_exe() else { return };
    let core = core_exe_next_to(&exe);
    // DETACHED_PROCESS | CREATE_NO_WINDOW: the core must outlive the UI.
    if let Err(e) = std::process::Command::new(&core).creation_flags(0x0000_0008 | 0x0800_0000).spawn() {
        eprintln!("could not start {}: {e}", core.display());
    }
}

/// Connects; when `may_launch_core`, starts the core once if the pipe is absent and waits for it.
async fn connect(pipe: &str, may_launch_core: bool) -> Option<IpcClient> {
    if let Ok(c) = IpcClient::connect(pipe).await {
        return Some(c);
    }
    if !may_launch_core {
        return None;
    }
    launch_core();
    let deadline = Instant::now() + CORE_START_TIMEOUT;
    while Instant::now() < deadline {
        tokio::time::sleep(CORE_START_POLL).await;
        if let Ok(c) = IpcClient::connect(pipe).await {
            return Some(c);
        }
    }
    None
}

async fn read_loop(app: &AppHandle, mut reader: IpcReader) {
    while let Ok(Some(line)) = reader.recv_raw().await {
        let Ok(value) = serde_json::from_str::<Value>(&line) else { continue };
        if is_shutdown(&value) {
            std::process::exit(0);
        }
        // Unknown variants are forwarded too; the frontend ignores what it does not know (D-012).
        let _ = app.emit("core", value);
    }
}

async fn run_link(app: AppHandle, link: Link, pipe: String, explicit_pipe: bool) {
    let mut may_launch_core = !explicit_pipe;
    loop {
        match connect(&pipe, may_launch_core).await {
            Some(client) => {
                may_launch_core = false;
                let (reader, mut writer) = client.into_halves();
                let (tx, mut rx) = mpsc::unbounded_channel::<String>();
                *link.lock().unwrap() = Some(tx);
                let write_task = tokio::spawn(async move {
                    while let Some(line) = rx.recv().await {
                        if writer.send_raw(&line).await.is_err() {
                            break;
                        }
                    }
                });
                let _ = app.emit("link", "connected");
                read_loop(&app, reader).await;
                *link.lock().unwrap() = None;
                write_task.abort();
                let _ = app.emit("link", "disconnected");
            }
            None => {
                // The core did not come up (or none was asked for): keep trying quietly.
                may_launch_core = false;
                let _ = app.emit("link", "disconnected");
            }
        }
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (pipe, explicit) = resolve_pipe(&args, || current_user_pipe_name(None));
    let link: Link = Arc::new(Mutex::new(None));
    let link_for_task = link.clone();
    tauri::Builder::default()
        .manage(link)
        .invoke_handler(tauri::generate_handler![send, link_state])
        .setup(move |app| {
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(run_link(handle, link_for_task, pipe, explicit));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running sala-ui");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn pipe_arg_is_read() {
        assert_eq!(pipe_arg(&args(&["ui", "--pipe", r"\\.\pipe\x"])), Some(r"\\.\pipe\x".to_string()));
        assert_eq!(pipe_arg(&args(&["ui"])), None);
        assert_eq!(pipe_arg(&args(&["ui", "--pipe"])), None);
        assert_eq!(pipe_arg(&args(&["ui", "--pipe", ""])), None);
    }

    #[test]
    fn pipe_falls_back_to_user_pipe_and_may_launch_core_only_then() {
        let (p, explicit) = resolve_pipe(&args(&["ui"]), || "default".into());
        assert_eq!((p.as_str(), explicit), ("default", false));
        let (p, explicit) = resolve_pipe(&args(&["ui", "--pipe", "mine"]), || unreachable!());
        assert_eq!((p.as_str(), explicit), ("mine", true));
    }

    #[test]
    fn core_exe_is_next_to_ui() {
        assert_eq!(core_exe_next_to(Path::new(r"C:\Concord\concord-ui.exe")), PathBuf::from(r"C:\Concord\concord.exe"));
    }

    #[test]
    fn commands_are_validated_and_become_one_line() {
        let line = command_line(serde_json::json!({"type": "SetMuted", "muted": true})).unwrap();
        assert_eq!(line, "{\"type\":\"SetMuted\",\"muted\":true}\n");
        assert!(command_line(serde_json::json!({"type": "Nope"})).is_err());
        assert!(command_line(serde_json::json!({"type": "SetMuted"})).is_err());
        assert!(command_line(serde_json::json!("x")).is_err());
    }

    #[test]
    fn shutdown_is_detected() {
        assert!(is_shutdown(&serde_json::json!({"type": "Shutdown"})));
        assert!(!is_shutdown(&serde_json::json!({"type": "Status"})));
        assert!(!is_shutdown(&serde_json::json!({})));
    }
}
