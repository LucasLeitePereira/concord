//! Lifecycle of the `sala-ui` process launched by the core (D-010).

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use sala_protocol::CoreToUi;
use tracing::{info, warn};

use crate::ipc::IpcServer;

pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(20);

#[derive(Debug, thiserror::Error)]
pub enum UiError {
    #[error("UI executable not found: {0}")]
    ExeNotFound(PathBuf),
    #[error("could not launch the UI: {0}")]
    Spawn(#[from] std::io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownOutcome {
    /// It exited on its own after `Shutdown`.
    Exited,
    /// It did not exit before the timeout and was killed.
    Killed,
    /// It was not running (never started or already gone).
    NotRunning,
}

pub struct UiProcess {
    child: Child,
}

/// `SALA_UI_EXE`, else `concord-ui.exe` next to the current executable.
fn ui_exe_next_to(exe: &Path) -> PathBuf {
    exe.parent().map(Path::to_path_buf).unwrap_or_default().join("concord-ui.exe")
}

pub fn resolve_ui_exe() -> PathBuf {
    if let Some(p) = std::env::var_os("SALA_UI_EXE") {
        return PathBuf::from(p);
    }
    ui_exe_next_to(&std::env::current_exe().unwrap_or_default())
}

impl UiProcess {
    /// Resolves the executable and launches it with `--pipe <pipe>`.
    pub fn launch(pipe: &str) -> Result<UiProcess, UiError> {
        UiProcess::spawn(&resolve_ui_exe(), pipe)
    }

    pub fn spawn(exe: &Path, pipe: &str) -> Result<UiProcess, UiError> {
        UiProcess::spawn_with_args(exe, pipe, &[])
    }

    /// Like [`UiProcess::spawn`] with extra arguments after `--pipe <pipe>` (tests).
    pub fn spawn_with_args(exe: &Path, pipe: &str, extra: &[&str]) -> Result<UiProcess, UiError> {
        if !exe.is_file() {
            return Err(UiError::ExeNotFound(exe.to_path_buf()));
        }
        let child = Command::new(exe).arg("--pipe").arg(pipe).args(extra).spawn()?;
        info!(pid = child.id(), "UI process launched");
        Ok(UiProcess { child })
    }

    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Asks the UI to quit through the pipe, waits up to `timeout`, then kills it.
    pub async fn shutdown(&mut self, server: &IpcServer, timeout: Duration) -> ShutdownOutcome {
        if !self.is_running() {
            return ShutdownOutcome::NotRunning;
        }
        server.broadcast(&CoreToUi::Shutdown);
        let deadline = Instant::now() + timeout;
        loop {
            if !self.is_running() {
                return ShutdownOutcome::Exited;
            }
            if Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(POLL).await;
        }
        warn!("UI did not exit in time; killing it");
        let _ = self.child.kill();
        let _ = self.child.wait();
        ShutdownOutcome::Killed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_concord_ui_next_to_core() {
        let core = Path::new(r"C:\Concord\concord.exe");
        assert_eq!(ui_exe_next_to(core), PathBuf::from(r"C:\Concord\concord-ui.exe"));
    }
}
