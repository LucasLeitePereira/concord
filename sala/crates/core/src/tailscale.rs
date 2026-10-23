//! Brings Tailscale up when the core starts (T-020): opens the tray app if it is not
//! running and runs `tailscale up` when the backend is stopped. Never blocks start-up
//! (the caller runs [`ensure_up`] in the background) and never shows a console window.
//!
//! The decision (state -> actions) is the pure [`plan`]; the rest only executes it.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tracing::{info, warn};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const DETACHED_PROCESS: u32 = 0x0000_0008;
const STATUS_TIMEOUT: Duration = Duration::from_secs(5);
const UP_TIMEOUT: Duration = Duration::from_secs(15);
/// How long to wait for `Running` after the actions (the user may be logging in).
const RUNNING_WAIT: Duration = Duration::from_secs(90);
const RUNNING_POLL: Duration = Duration::from_secs(2);

/// Tailscale's `BackendState`, reduced to what matters here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Running,
    Stopped,
    NeedsLogin,
    /// Anything else (`Starting`, `NoState`, ...) or no answer: wait, do nothing.
    Other,
}

/// What [`plan`] asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Start `tailscale-ipn.exe` (the tray app).
    StartTrayApp,
    /// Run `tailscale up`.
    Up,
    /// Log that the user has to sign in on the tray app.
    NoteNeedsLogin,
}

/// Pure decision: tray app and backend state -> actions, in order.
pub fn plan(tray_app_running: bool, backend: Backend) -> Vec<Action> {
    let mut out = Vec::new();
    if !tray_app_running {
        out.push(Action::StartTrayApp);
    }
    match backend {
        Backend::Stopped => out.push(Action::Up),
        Backend::NeedsLogin => out.push(Action::NoteNeedsLogin),
        Backend::Running | Backend::Other => {}
    }
    out
}

/// `BackendState` from the output of `tailscale status --json` (no JSON dependency:
/// the field is a plain string at the top level).
pub fn parse_backend(json: &str) -> Backend {
    let Some(i) = json.find("\"BackendState\"") else { return Backend::Other };
    let rest = json[i + "\"BackendState\"".len()..].trim_start();
    let Some(rest) = rest.strip_prefix(':') else { return Backend::Other };
    let rest = rest.trim_start();
    let Some(rest) = rest.strip_prefix('"') else { return Backend::Other };
    match rest.split('"').next().unwrap_or("") {
        "Running" => Backend::Running,
        "Stopped" => Backend::Stopped,
        "NeedsLogin" => Backend::NeedsLogin,
        _ => Backend::Other,
    }
}

fn install_dir() -> Option<PathBuf> {
    let base = std::env::var_os("ProgramFiles")?;
    Some(PathBuf::from(base).join("Tailscale"))
}

/// Runs a command without a window, with a timeout; returns stdout on success.
fn run_capture(mut cmd: Command, timeout: Duration) -> Option<String> {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(CREATE_NO_WINDOW).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = cmd.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    // Read on a thread so a full pipe cannot stall the wait loop.
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = reader.join().ok()?;
                return status.success().then_some(out);
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

fn tray_app_running() -> bool {
    let mut cmd = Command::new("tasklist");
    cmd.args(["/FI", "IMAGENAME eq tailscale-ipn.exe", "/NH", "/FO", "CSV"]);
    run_capture(cmd, STATUS_TIMEOUT).is_some_and(|o| o.to_ascii_lowercase().contains("tailscale-ipn.exe"))
}

fn backend(cli: &PathBuf) -> Backend {
    let mut cmd = Command::new(cli);
    cmd.args(["status", "--json"]);
    run_capture(cmd, STATUS_TIMEOUT).map_or(Backend::Other, |o| parse_backend(&o))
}

fn start_tray_app(dir: &std::path::Path) {
    use std::os::windows::process::CommandExt;
    let exe = dir.join("tailscale-ipn.exe");
    match Command::new(&exe)
        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(_) => info!("tailscale: tray app started"),
        Err(e) => warn!("tailscale: could not start the tray app: {e}"),
    }
}

/// Blocking: does what [`plan`] says and waits (bounded) for `Running`.
/// Returns true when the backend ended up `Running`.
fn ensure_up_blocking() -> bool {
    let Some(dir) = install_dir().filter(|d| d.join("tailscale.exe").exists()) else {
        info!("tailscale: not installed; skipping");
        return false;
    };
    let cli = dir.join("tailscale.exe");
    let state = backend(&cli);
    for action in plan(tray_app_running(), state) {
        match action {
            Action::StartTrayApp => start_tray_app(&dir),
            Action::Up => {
                let mut cmd = Command::new(&cli);
                cmd.arg("up");
                match run_capture(cmd, UP_TIMEOUT) {
                    Some(_) => info!("tailscale: up done"),
                    None => warn!("tailscale: `tailscale up` failed or timed out"),
                }
            }
            Action::NoteNeedsLogin => info!("tailscale: needs login; sign in on the Tailscale tray app"),
        }
    }
    let deadline = Instant::now() + RUNNING_WAIT;
    loop {
        if backend(&cli) == Backend::Running {
            info!("tailscale: running");
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(RUNNING_POLL);
    }
}

/// Background task: true when Tailscale is `Running` at the end.
pub async fn ensure_up() -> bool {
    tokio::task::spawn_blocking(ensure_up_blocking).await.unwrap_or(false)
}

/// Blocking: `tailscale down` and closes the tray app (the service stays installed).
fn shut_down_blocking() {
    let Some(dir) = install_dir().filter(|d| d.join("tailscale.exe").exists()) else { return };
    let mut cmd = Command::new(dir.join("tailscale.exe"));
    cmd.arg("down");
    match run_capture(cmd, UP_TIMEOUT) {
        Some(_) => info!("tailscale: down done"),
        None => warn!("tailscale: `tailscale down` failed or timed out"),
    }
    let mut cmd = Command::new("taskkill");
    cmd.args(["/F", "/IM", "tailscale-ipn.exe"]);
    if run_capture(cmd, STATUS_TIMEOUT).is_some() {
        info!("tailscale: tray app closed");
    }
}

/// On quit: Tailscale goes down together with the app (user's request, D-018).
pub async fn shut_down() {
    let _ = tokio::task::spawn_blocking(shut_down_blocking).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_state_to_actions() {
        use Action::*;
        assert_eq!(plan(true, Backend::Running), vec![]);
        assert_eq!(plan(true, Backend::Stopped), vec![Up]);
        assert_eq!(plan(false, Backend::Stopped), vec![StartTrayApp, Up]);
        assert_eq!(plan(false, Backend::NeedsLogin), vec![StartTrayApp, NoteNeedsLogin]);
        assert_eq!(plan(true, Backend::NeedsLogin), vec![NoteNeedsLogin]);
        // Control: unknown/starting state never runs `up`.
        assert_eq!(plan(true, Backend::Other), vec![]);
        assert_eq!(plan(false, Backend::Other), vec![StartTrayApp]);
    }

    #[test]
    fn parses_backend_state() {
        assert_eq!(parse_backend("{\n  \"Version\": \"1\",\n  \"BackendState\": \"Stopped\",\n"), Backend::Stopped);
        assert_eq!(parse_backend("{\"BackendState\":\"Running\"}"), Backend::Running);
        assert_eq!(parse_backend("{\"BackendState\": \"NeedsLogin\"}"), Backend::NeedsLogin);
        assert_eq!(parse_backend("{\"BackendState\": \"Starting\"}"), Backend::Other);
        assert_eq!(parse_backend("not json"), Backend::Other);
        assert_eq!(parse_backend(""), Backend::Other);
    }
}
