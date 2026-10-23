//! Integration tests for the IPC named pipe and the UI process lifecycle (T-011, D-010).
//!
//! The `UiProcess` tests need the `fake_ui` example binary. `cargo test` builds
//! examples; with `--test ipc` alone it may not, so the helper builds it on demand.
#![cfg(windows)]

use std::future::Future;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use sala_core::ipc::{IpcClient, IpcError, IpcServer, MAX_LINE_BYTES};
use sala_core::ui_process::{ShutdownOutcome, UiError, UiProcess};
use sala_protocol::{CoreToUi, UiToCore};

const T: Duration = Duration::from_secs(10);

fn unique_pipe() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    format!(r"\\.\pipe\sala-test-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed))
}

async fn within<F: Future>(f: F) -> F::Output {
    tokio::time::timeout(T, f).await.expect("timed out")
}

fn status() -> CoreToUi {
    CoreToUi::Status { muted: true, deafened: false, game_mode: false, in_room: true }
}

fn fake_ui_exe() -> PathBuf {
    let profile_dir = std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().to_path_buf();
    let exe = profile_dir.join("examples").join("fake_ui.exe");
    static BUILT: OnceLock<()> = OnceLock::new();
    BUILT.get_or_init(|| {
        if !exe.is_file() {
            let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
            let st = std::process::Command::new(cargo)
                .args(["build", "-p", "sala-core", "--example", "fake_ui"])
                .status()
                .unwrap();
            assert!(st.success());
        }
    });
    assert!(exe.is_file(), "fake_ui not found at {exe:?}");
    exe
}

#[tokio::test]
async fn command_and_reply_roundtrip() {
    let name = unique_pipe();
    let (server, mut rx) = IpcServer::start(&name).unwrap();
    let mut c = IpcClient::connect(&name).await.unwrap();
    c.send(&UiToCore::SendChat { text: "oi".into() }).await.unwrap();
    let (id, cmd) = within(rx.recv()).await.unwrap();
    assert_eq!(cmd, UiToCore::SendChat { text: "oi".into() });
    assert!(server.send_to(id, &status()));
    assert_eq!(within(c.recv()).await.unwrap(), Some(status()));
    assert!(!server.send_to(id + 1000, &status()));
    server.shutdown().await;
    assert_eq!(within(c.recv()).await.unwrap(), None);
}

#[tokio::test]
async fn broadcast_reaches_all_clients() {
    let name = unique_pipe();
    let (server, mut rx) = IpcServer::start(&name).unwrap();
    let mut a = IpcClient::connect(&name).await.unwrap();
    let mut b = IpcClient::connect(&name).await.unwrap();
    // Wait until the server has registered both (each sends a command).
    a.send(&UiToCore::GetState).await.unwrap();
    b.send(&UiToCore::GetState).await.unwrap();
    let (ia, _) = within(rx.recv()).await.unwrap();
    let (ib, _) = within(rx.recv()).await.unwrap();
    assert_ne!(ia, ib);
    assert_eq!(server.broadcast(&status()), 2);
    assert_eq!(within(a.recv()).await.unwrap(), Some(status()));
    assert_eq!(within(b.recv()).await.unwrap(), Some(status()));
    server.shutdown().await;
}

#[tokio::test]
async fn invalid_line_gives_error_and_connection_continues() {
    let name = unique_pipe();
    let (server, mut rx) = IpcServer::start(&name).unwrap();
    let mut a = IpcClient::connect(&name).await.unwrap();
    let mut b = IpcClient::connect(&name).await.unwrap();
    a.send_raw("this is not json\n{\"type\":\"Nope\"}\n").await.unwrap();
    for _ in 0..2 {
        assert!(matches!(within(a.recv()).await.unwrap(), Some(CoreToUi::Error { .. })));
    }
    a.send(&UiToCore::HangUp).await.unwrap();
    assert_eq!(within(rx.recv()).await.unwrap().1, UiToCore::HangUp);
    // The other client got no Error.
    b.send(&UiToCore::GetState).await.unwrap();
    assert_eq!(within(rx.recv()).await.unwrap().1, UiToCore::GetState);
    server.broadcast(&status());
    assert_eq!(within(b.recv()).await.unwrap(), Some(status()));
    server.shutdown().await;
}

#[tokio::test]
async fn oversized_line_disconnects_only_that_client() {
    let name = unique_pipe();
    let (server, mut rx) = IpcServer::start(&name).unwrap();
    let mut bad = IpcClient::connect(&name).await.unwrap();
    let mut good = IpcClient::connect(&name).await.unwrap();
    let huge = "x".repeat(MAX_LINE_BYTES + 10);
    let _ = bad.send_raw(&huge).await; // may fail once the server closes
    let r = within(bad.recv()).await;
    assert!(matches!(r, Ok(None) | Err(_)), "got {r:?}");
    good.send(&UiToCore::GetState).await.unwrap();
    assert_eq!(within(rx.recv()).await.unwrap().1, UiToCore::GetState);
    server.shutdown().await;
}

#[tokio::test]
async fn second_server_same_name_is_already_running() {
    let name = unique_pipe();
    let (server, _rx) = IpcServer::start(&name).unwrap();
    let second = IpcServer::start(&name);
    assert!(matches!(second, Err(IpcError::AlreadyRunning)), "got {:?}", second.err());
    server.shutdown().await;
}

#[tokio::test]
async fn dropped_client_does_not_kill_server() {
    let name = unique_pipe();
    let (server, mut rx) = IpcServer::start(&name).unwrap();
    {
        let mut c = IpcClient::connect(&name).await.unwrap();
        c.send(&UiToCore::GetState).await.unwrap();
        within(rx.recv()).await.unwrap();
    }
    let t0 = Instant::now();
    while server.client_count() != 0 {
        assert!(t0.elapsed() < T, "client never removed");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut d = IpcClient::connect(&name).await.unwrap();
    d.send(&UiToCore::HangUp).await.unwrap();
    assert_eq!(within(rx.recv()).await.unwrap().1, UiToCore::HangUp);
    assert_eq!(server.broadcast(&status()), 1);
    server.shutdown().await;
}

#[tokio::test]
async fn ui_process_exits_on_shutdown() {
    let name = unique_pipe();
    let (server, mut rx) = IpcServer::start(&name).unwrap();
    let mut ui = UiProcess::spawn(&fake_ui_exe(), &name).unwrap();
    assert!(ui.is_running());
    // fake_ui announces itself with GetState.
    assert_eq!(within(rx.recv()).await.unwrap().1, UiToCore::GetState);
    let out = within(ui.shutdown(&server, Duration::from_secs(5))).await;
    assert_eq!(out, ShutdownOutcome::Exited);
    assert!(!ui.is_running());
    assert_eq!(ui.shutdown(&server, Duration::from_secs(1)).await, ShutdownOutcome::NotRunning);
    server.shutdown().await;
}

#[tokio::test]
async fn ui_process_ignoring_shutdown_is_killed_after_timeout() {
    let name = unique_pipe();
    let (server, mut rx) = IpcServer::start(&name).unwrap();
    let mut ui = UiProcess::spawn_with_args(&fake_ui_exe(), &name, &["--ignore-shutdown"]).unwrap();
    // Wait for it to be connected so the Shutdown is really ignored, not unseen.
    assert_eq!(within(rx.recv()).await.unwrap().1, UiToCore::GetState);
    let t0 = Instant::now();
    let out = within(ui.shutdown(&server, Duration::from_millis(500))).await;
    let el = t0.elapsed();
    assert_eq!(out, ShutdownOutcome::Killed);
    assert!(el >= Duration::from_millis(450) && el < Duration::from_secs(4), "elapsed {el:?}");
    assert!(!ui.is_running());
    server.shutdown().await;
}

#[tokio::test]
async fn missing_ui_exe_is_a_typed_error() {
    let r = UiProcess::spawn(std::path::Path::new(r"C:\definitely\not\here\sala-ui.exe"), "x");
    assert!(matches!(r, Err(UiError::ExeNotFound(_))));
}
