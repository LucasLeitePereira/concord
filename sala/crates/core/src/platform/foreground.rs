//! Foreground window probe (D-009): window -> PID -> image path, plus whether
//! the window covers its whole monitor.

use std::mem;

use windows_sys::Win32::Foundation::{CloseHandle, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId,
};

use crate::game_detector::ForegroundSample;

/// Desktop and shell windows count as "no window in the foreground".
const SHELL_CLASSES: &[&str] = &["Progman", "WorkerW", "Shell_TrayWnd"];

fn is_shell_class(class: &str) -> bool {
    SHELL_CLASSES.iter().any(|c| c.eq_ignore_ascii_case(class))
}

/// The window rectangle contains the whole monitor rectangle.
fn rect_covers(window: &RECT, monitor: &RECT) -> bool {
    window.left <= monitor.left
        && window.top <= monitor.top
        && window.right >= monitor.right
        && window.bottom >= monitor.bottom
}

/// Sample for a valid window whose image path may be unknown: a protected
/// process (anti-cheat) that refuses `OpenProcess` still counts as a window,
/// with an empty path, so a protected full-screen game stays positive.
fn build_sample(exe_path: Option<String>, covers_monitor: bool) -> ForegroundSample {
    ForegroundSample { exe_path: exe_path.unwrap_or_default(), covers_monitor }
}

/// Image path of `pid`, or `None` (logged at debug: runs every 2 s).
///
/// # Safety
/// Plain Win32 queries; the process handle is closed on all paths.
unsafe fn image_path(pid: u32) -> Option<String> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            tracing::debug!(pid, "OpenProcess failed for the foreground window");
            return None;
        }
        let mut buf = vec![0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(process, 0, buf.as_mut_ptr(), &mut len);
        CloseHandle(process);
        if ok == 0 || len == 0 {
            tracing::debug!(pid, "QueryFullProcessImageNameW failed for the foreground window");
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// Samples the foreground window. `None` only when there is no window
/// (desktop, shell, lock screen). If there is a window but its process cannot
/// be inspected (protected process, anti-cheat), the sample has an empty
/// `exe_path` and a real `covers_monitor`, so it is not mistaken for "no window".
pub fn probe() -> Option<ForegroundSample> {
    // SAFETY: plain Win32 queries; every handle is checked before use.
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return None;
        }

        let mut class = [0u16; 64];
        let n = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32);
        if n > 0 && is_shell_class(&String::from_utf16_lossy(&class[..n as usize])) {
            return None;
        }

        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        let exe_path = if pid == 0 { None } else { image_path(pid) };

        let mut window: RECT = mem::zeroed();
        let mut info: MONITORINFO = mem::zeroed();
        info.cbSize = mem::size_of::<MONITORINFO>() as u32;
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let covers_monitor = GetWindowRect(hwnd, &mut window) != 0
            && !monitor.is_null()
            && GetMonitorInfoW(monitor, &mut info) != 0
            && rect_covers(&window, &info.rcMonitor);

        Some(build_sample(exe_path, covers_monitor))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT { left, top, right, bottom }
    }

    #[test]
    fn covering_rules() {
        let mon = r(0, 0, 1920, 1080);
        assert!(rect_covers(&r(0, 0, 1920, 1080), &mon));
        assert!(rect_covers(&r(-8, -8, 1928, 1088), &mon)); // maximised with borders
        assert!(!rect_covers(&r(0, 0, 1920, 1040), &mon)); // maximised above the taskbar
        assert!(!rect_covers(&r(100, 100, 1000, 800), &mon)); // windowed
        // second monitor at a positive offset
        let mon2 = r(1920, 0, 3840, 1080);
        assert!(rect_covers(&r(1920, 0, 3840, 1080), &mon2));
        assert!(!rect_covers(&r(0, 0, 1920, 1080), &mon2));
    }

    #[test]
    fn unknown_path_still_yields_a_sample() {
        let s = build_sample(None, true);
        assert_eq!(s, ForegroundSample { exe_path: String::new(), covers_monitor: true });
        // negative control: a known path is kept as is
        let s = build_sample(Some("x.exe".into()), false);
        assert_eq!(s.exe_path, "x.exe");
        assert!(!s.covers_monitor);
    }

    #[test]
    fn shell_classes() {
        assert!(is_shell_class("Progman"));
        assert!(is_shell_class("workerw"));
        assert!(is_shell_class("Shell_TrayWnd"));
        assert!(!is_shell_class("Chrome_WidgetWin_1"));
    }

    /// Needs an interactive desktop that lets a new process take the
    /// foreground: ignored by default (fails in CI / service sessions).
    /// Run with `cargo test -p sala-core -- --ignored probe_sees_fake_game`.
    #[test]
    #[ignore = "needs an interactive desktop with foreground rights"]
    fn probe_sees_fake_game() {
        let exe = std::env::current_exe().unwrap();
        let fake = exe.parent().unwrap().parent().unwrap().join("examples").join("fake_game.exe");
        assert!(fake.exists(), "build examples first: {}", fake.display());
        let mut child = std::process::Command::new(&fake).args(["--seconds", "6"]).spawn().unwrap();
        let mut seen = None;
        for _ in 0..30 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if let Some(s) = probe()
                && s.exe_path.to_ascii_lowercase().ends_with("fake_game.exe")
            {
                seen = Some(s);
                break;
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        let s = seen.expect("fake_game never became the foreground window");
        assert!(s.covers_monitor, "full-screen window must cover the monitor");
    }

    #[test]
    fn probe_is_coherent() {
        // Must not panic whatever the session state; when it answers, the
        // path is an absolute .exe path.
        if let Some(s) = probe()
            && !s.exe_path.is_empty()
        {
            assert!(s.exe_path.to_ascii_lowercase().ends_with(".exe"), "{}", s.exe_path);
            assert!(s.exe_path.contains('\\'), "{}", s.exe_path);
        }
    }
}
