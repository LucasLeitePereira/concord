//! Borderless window covering the primary monitor, brought to the foreground.
//! Used to test automatic game detection (full-screen foreground window).
//! Closes with Esc or after `--seconds N`.
//!
//!   cargo run --example fake_game -- --seconds 30

#[cfg(windows)]
fn main() {
    use std::mem;
    use std::ptr;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::{BLACK_BRUSH, GetStockObject};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_ESCAPE, VK_MENU, keybd_event, KEYEVENTF_KEYUP};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
        GetSystemMetrics, MSG, PostQuitMessage, RegisterClassExW, SM_CXSCREEN, SM_CYSCREEN,
        SW_SHOW, SetForegroundWindow, SetTimer, ShowWindow, TranslateMessage, WM_DESTROY,
        WM_KEYDOWN, WM_TIMER, WNDCLASSEXW, WS_POPUP, WS_VISIBLE,
    };

    unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        // SAFETY: standard window-procedure handling.
        unsafe {
            match msg {
                WM_KEYDOWN if wp == VK_ESCAPE as usize => {
                    DestroyWindow(hwnd);
                    0
                }
                WM_TIMER => {
                    DestroyWindow(hwnd);
                    0
                }
                WM_DESTROY => {
                    PostQuitMessage(0);
                    0
                }
                _ => DefWindowProcW(hwnd, msg, wp, lp),
            }
        }
    }

    let args: Vec<String> = std::env::args().collect();
    let seconds: Option<u32> = args
        .iter()
        .position(|a| a == "--seconds")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok());

    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    let class = wide("SalaFakeGame");
    let title = wide("fake_game");
    // SAFETY: plain window creation and message loop on this thread.
    unsafe {
        let hinstance = GetModuleHandleW(ptr::null());
        let mut wc: WNDCLASSEXW = mem::zeroed();
        wc.cbSize = mem::size_of::<WNDCLASSEXW>() as u32;
        wc.lpfnWndProc = Some(proc);
        wc.hInstance = hinstance;
        wc.lpszClassName = class.as_ptr();
        wc.hbrBackground = GetStockObject(BLACK_BRUSH) as _;
        assert!(RegisterClassExW(&wc) != 0, "RegisterClassExW failed");
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_POPUP | WS_VISIBLE,
            0,
            0,
            GetSystemMetrics(SM_CXSCREEN),
            GetSystemMetrics(SM_CYSCREEN),
            ptr::null_mut(),
            ptr::null_mut(),
            hinstance,
            ptr::null(),
        );
        assert!(!hwnd.is_null(), "CreateWindowExW failed");
        ShowWindow(hwnd, SW_SHOW);
        // A synthetic Alt tap lifts the foreground lock when launched from a
        // process that is not itself in the foreground.
        keybd_event(VK_MENU as u8, 0, 0, 0);
        keybd_event(VK_MENU as u8, 0, KEYEVENTF_KEYUP, 0);
        if SetForegroundWindow(hwnd) == 0 {
            eprintln!("warning: Windows refused to bring the window to the foreground; click it");
        }
        if let Some(s) = seconds {
            SetTimer(hwnd, 1, s.saturating_mul(1000), None);
        }
        println!("fake_game: full-screen window up (Esc closes{})", match seconds {
            Some(s) => format!(", or after {s} s"),
            None => String::new(),
        });
        let mut msg: MSG = mem::zeroed();
        while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("fake_game only runs on Windows");
}
