//! The platform thread: hidden window, message loop, tray icon, hotkeys,
//! key polling and the tray menu. Everything here runs on that one thread;
//! `STATE` is thread-local and only touched from the window procedure and
//! `run`.

use std::cell::RefCell;
use std::io;
use std::mem;
use std::panic::{self, AssertUnwindSafe};
use std::ptr;
use std::sync::mpsc::{Receiver, Sender};

use windows_sys::Win32::Foundation::{GetLastError, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey,
    UnregisterHotKey,
};
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu,
    DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, HICON, KillTimer, MF_GRAYED,
    MF_SEPARATOR, MF_STRING, MSG, PostMessageW, PostQuitMessage, RegisterClassExW,
    RegisterWindowMessageW, SetForegroundWindow, SetTimer, TPM_BOTTOMALIGN, TPM_LEFTALIGN,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WM_APP, WM_CONTEXTMENU,
    WM_DESTROY, WM_HOTKEY, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WM_TIMER, WM_USER, WNDCLASSEXW,
};

use super::hotkey::{Hotkey, HotkeyKey};
use super::icon::{self, IconKind};
use super::{Command, EventSender, HotkeyAction, MenuItem, PlatformEvent, TrayState, copy_wide, wide};

/// Posted by `Platform` after queuing a command.
pub const WM_APP_COMMAND: u32 = WM_APP + 1;
/// Tray callback message.
const WM_APP_TRAY: u32 = WM_APP + 2;
/// Balloon/toast clicked (`NIN_BALLOONUSERCLICK`; legacy callback style).
const NIN_BALLOONUSERCLICK: u32 = WM_USER + 5;
const TRAY_ID: u32 = 1;
const POLL_TIMER: usize = 1;
const POLL_MS: u32 = 15;
const ERROR_CLASS_ALREADY_EXISTS: u32 = 1410;
const CLASS_NAME: &str = "SalaPlatformWindow";
const NOTIFICATION_BODY: &str = "Nova mensagem no chat";

/// A key/button watched by the 15 ms timer. `action == None` is push-to-talk.
struct Polled {
    action: Option<HotkeyAction>,
    hotkey: Hotkey,
    down: bool,
}

struct State {
    hwnd: HWND,
    commands: Receiver<Command>,
    events: EventSender,
    tray: TrayState,
    icon: HICON,
    icon_kind: Option<IconKind>,
    icon_added: bool,
    taskbar_created: u32,
    /// Shortcuts as given, for the menu's right-hand labels.
    shortcuts: Vec<(HotkeyAction, Hotkey)>,
    registered: Vec<(i32, HotkeyAction)>,
    polled: Vec<Polled>,
    timer_on: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Runs `f` on the state; `None` if there is none yet or it is already
/// borrowed (re-entrant message dispatch), in which case the caller falls
/// back to the default handling.
fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|c| {
        let mut b = c.try_borrow_mut().ok()?;
        b.as_mut().map(f)
    })
}

fn key_down(vk: u16) -> bool {
    // SAFETY: plain query.
    unsafe { (GetAsyncKeyState(i32::from(vk)) as u16 & 0x8000) != 0 }
}

fn nid(hwnd: HWND) -> NOTIFYICONDATAW {
    // SAFETY: NOTIFYICONDATAW is plain data; all-zero is a valid empty value.
    let mut n: NOTIFYICONDATAW = unsafe { mem::zeroed() };
    n.cbSize = mem::size_of::<NOTIFYICONDATAW>() as u32;
    n.hWnd = hwnd;
    n.uID = TRAY_ID;
    n
}

impl State {
    fn tooltip(&self) -> String {
        let mut s = String::from("Concord");
        if self.tray.game_mode {
            s.push_str(" · modo jogo");
        }
        if self.tray.deafened {
            s.push_str(" · silenciado");
        } else if self.tray.muted {
            s.push_str(" · mutado");
        }
        s
    }

    fn emit(&self, ev: PlatformEvent) {
        let _ = self.events.send(ev);
    }

    /// Adds the icon (first time or after the Explorer restarted) or updates it.
    fn update_tray(&mut self) {
        let kind = IconKind::of(&self.tray);
        let mut old = ptr::null_mut();
        if self.icon_kind != Some(kind) || self.icon.is_null() {
            if let Some(new) = icon::create(kind) {
                old = mem::replace(&mut self.icon, new);
                self.icon_kind = Some(kind);
            } else {
                tracing::warn!("could not create the tray icon bitmap");
            }
        }
        let mut n = nid(self.hwnd);
        n.uFlags = NIF_ICON | NIF_TIP | NIF_MESSAGE;
        n.uCallbackMessage = WM_APP_TRAY;
        n.hIcon = self.icon;
        copy_wide(&mut n.szTip, &self.tooltip());
        let op = if self.icon_added { NIM_MODIFY } else { NIM_ADD };
        // SAFETY: `n` is fully initialised and outlives the call.
        let ok = unsafe { Shell_NotifyIconW(op, &n) } != 0;
        if ok {
            self.icon_added = true;
        } else {
            self.icon_added = false;
            tracing::warn!("Shell_NotifyIconW failed; will retry on the next update");
        }
        if !old.is_null() {
            // SAFETY: `old` was created by us and the shell now holds a copy.
            unsafe { DestroyIcon(old) };
        }
    }

    fn notify(&self, sender: &str) {
        if !self.icon_added {
            return;
        }
        let mut n = nid(self.hwnd);
        n.uFlags = NIF_INFO;
        let title = if sender.trim().is_empty() { "Concord" } else { sender };
        copy_wide(&mut n.szInfoTitle, title);
        copy_wide(&mut n.szInfo, NOTIFICATION_BODY);
        n.dwInfoFlags = NIIF_INFO;
        // SAFETY: as in `update_tray`.
        if unsafe { Shell_NotifyIconW(NIM_MODIFY, &n) } == 0 {
            tracing::warn!("could not show the notification");
        }
    }

    fn sync_timer(&mut self) {
        let want = !self.polled.is_empty();
        // SAFETY: timer on our own window.
        unsafe {
            if want && !self.timer_on {
                self.timer_on = SetTimer(self.hwnd, POLL_TIMER, POLL_MS, None) != 0;
            } else if !want && self.timer_on {
                KillTimer(self.hwnd, POLL_TIMER);
                self.timer_on = false;
            }
        }
    }

    fn apply_hotkeys(&mut self, list: Vec<(HotkeyAction, Hotkey)>) {
        for (id, _) in self.registered.drain(..) {
            // SAFETY: ids were registered on this window.
            unsafe { UnregisterHotKey(self.hwnd, id) };
        }
        self.polled.retain(|p| p.action.is_none());
        for (i, (action, hk)) in list.iter().enumerate() {
            match hk.key {
                HotkeyKey::Mouse(_) => {
                    // Start from the current state so a button already held
                    // does not fire the action.
                    let down = hk.is_down(key_down);
                    self.polled.push(Polled { action: Some(*action), hotkey: *hk, down });
                }
                HotkeyKey::Vk(vk) => {
                    let id = i as i32 + 1;
                    let m = hk.modifiers;
                    let mut mods = MOD_NOREPEAT;
                    for (on, flag) in
                        [(m.ctrl, MOD_CONTROL), (m.alt, MOD_ALT), (m.shift, MOD_SHIFT), (m.win, MOD_WIN)]
                    {
                        if on {
                            mods |= flag;
                        }
                    }
                    // SAFETY: plain registration on our window.
                    if unsafe { RegisterHotKey(self.hwnd, id, mods, u32::from(vk)) } != 0 {
                        self.registered.push((id, *action));
                    } else {
                        // SAFETY: reads the thread's last error.
                        let error_code = unsafe { GetLastError() };
                        // Reported once, by the controller, with the action name.
                        self.emit(PlatformEvent::HotkeyRegistrationFailed {
                            action: *action,
                            hotkey: *hk,
                            error_code,
                        });
                    }
                }
            }
        }
        self.shortcuts = list;
        self.sync_timer();
    }

    fn set_ptt(&mut self, hotkey: Option<Hotkey>) {
        if self.polled.iter().any(|p| p.action.is_none() && p.down) {
            self.emit(PlatformEvent::PttReleased);
        }
        self.polled.retain(|p| p.action.is_some());
        if let Some(hotkey) = hotkey {
            self.polled.push(Polled { action: None, hotkey, down: false });
        }
        self.sync_timer();
    }

    fn on_timer(&mut self) {
        let mut out = Vec::new();
        for p in &mut self.polled {
            let now = p.hotkey.is_down(key_down);
            if now == p.down {
                continue;
            }
            p.down = now;
            match (p.action, now) {
                (Some(a), true) => out.push(PlatformEvent::Hotkey(a)),
                (Some(_), false) => {}
                (None, true) => out.push(PlatformEvent::PttPressed),
                (None, false) => out.push(PlatformEvent::PttReleased),
            }
        }
        for ev in out {
            self.emit(ev);
        }
    }

    fn apply(&mut self, cmd: Command) {
        match cmd {
            Command::Tray(t) => {
                self.tray = t;
                self.update_tray();
            }
            Command::Notify { sender } => self.notify(&sender),
            Command::Hotkeys(l) => self.apply_hotkeys(l),
            Command::Ptt(h) => self.set_ptt(h),
            Command::Shutdown => {}
        }
    }

    fn cleanup(&mut self) {
        if self.icon_added {
            let n = nid(self.hwnd);
            // SAFETY: removes our own icon.
            unsafe { Shell_NotifyIconW(NIM_DELETE, &n) };
            self.icon_added = false;
        }
        for (id, _) in self.registered.drain(..) {
            // SAFETY: ids were registered on this window.
            unsafe { UnregisterHotKey(self.hwnd, id) };
        }
        self.polled.clear();
        self.sync_timer();
        if !self.icon.is_null() {
            // SAFETY: created by us, no longer referenced.
            unsafe { DestroyIcon(self.icon) };
            self.icon = ptr::null_mut();
        }
    }
}

// ---------------------------------------------------------------- menu

#[derive(Debug, PartialEq, Eq)]
pub(super) enum MenuLine {
    /// Greyed informational text.
    Info(String),
    Separator,
    Item(MenuItem, String),
}

fn label(text: &str, action: HotkeyAction, shortcuts: &[(HotkeyAction, Hotkey)]) -> String {
    match shortcuts.iter().find(|(a, _)| *a == action) {
        Some((_, hk)) => format!("{text}\t{hk}"),
        None => text.to_string(),
    }
}

/// Informational lines go through the menu's mnemonic parser: `&` must be doubled.
fn escape_amp(s: &str) -> String {
    s.replace('&', "&&")
}

/// Menu content in the order of `ModoJogo.dc.html`.
pub(super) fn menu_lines(tray: &TrayState, shortcuts: &[(HotkeyAction, Hotkey)]) -> Vec<MenuLine> {
    let header = if tray.game_mode { "Concord · Modo jogo" } else { "Concord" };
    let call = match &tray.call {
        Some(c) => escape_amp(c),
        None => "Fora de chamada".to_string(),
    };
    let mute = if tray.muted { "Desmutar-se" } else { "Mutar-se" };
    let deafen = if tray.deafened { "Voltar a ouvir" } else { "Silenciar" };
    let game = if tray.game_mode { "Sair do modo jogo" } else { "Entrar no modo jogo" };
    vec![
        MenuLine::Info(header.to_string()),
        MenuLine::Info(call),
        MenuLine::Separator,
        MenuLine::Item(MenuItem::ToggleMute, label(mute, HotkeyAction::ToggleMute, shortcuts)),
        MenuLine::Item(MenuItem::ToggleDeafen, label(deafen, HotkeyAction::ToggleDeafen, shortcuts)),
        MenuLine::Item(MenuItem::ToggleGameMode, label(game, HotkeyAction::ToggleGameMode, shortcuts)),
        MenuLine::Item(MenuItem::OpenSala, "Abrir o Concord".to_string()),
        MenuLine::Separator,
        MenuLine::Item(MenuItem::QuitSala, "Fechar o Concord".to_string()),
    ]
}

const MENU_IDS: [MenuItem; 5] = [
    MenuItem::ToggleMute,
    MenuItem::ToggleDeafen,
    MenuItem::ToggleGameMode,
    MenuItem::OpenSala,
    MenuItem::QuitSala,
];

fn show_menu(hwnd: HWND) {
    let Some(lines) = with_state(|s| menu_lines(&s.tray, &s.shortcuts)) else {
        return;
    };
    // The popup menu runs a nested message loop: no state borrow may be held.
    // SAFETY: standard popup-menu sequence on our own window.
    let chosen = unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        for line in &lines {
            match line {
                MenuLine::Info(t) => {
                    AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, wide(t).as_ptr());
                }
                MenuLine::Separator => {
                    AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
                }
                MenuLine::Item(item, t) => {
                    let id = MENU_IDS.iter().position(|i| i == item).unwrap_or(0) + 1;
                    AppendMenuW(menu, MF_STRING, id, wide(t).as_ptr());
                }
            }
        }
        // Required so the menu closes when the user clicks elsewhere.
        SetForegroundWindow(hwnd);
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_LEFTALIGN,
            pt.x,
            pt.y,
            0,
            hwnd,
            ptr::null(),
        );
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);
        cmd
    };
    if chosen > 0
        && let Some(item) = MENU_IDS.get(chosen as usize - 1)
    {
        with_state(|s| s.emit(PlatformEvent::Menu(*item)));
    }
}

// --------------------------------------------------------- window proc

fn handle_commands(hwnd: HWND) {
    let cmds: Vec<Command> =
        with_state(|s| s.commands.try_iter().collect()).unwrap_or_default();
    for cmd in cmds {
        if matches!(cmd, Command::Shutdown) {
            // WM_DESTROY re-enters the window procedure: no borrow held here.
            // SAFETY: destroying our own window from its thread.
            unsafe { DestroyWindow(hwnd) };
            return;
        }
        with_state(|s| s.apply(cmd));
    }
}

fn handle(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    if let Some(tc) = with_state(|s| s.taskbar_created)
        && tc != 0
        && msg == tc
    {
        // Explorer restarted: the icon is gone, add it again.
        with_state(|s| {
            s.icon_added = false;
            s.update_tray();
        });
        return Some(0);
    }
    match msg {
        WM_APP_COMMAND => {
            handle_commands(hwnd);
            Some(0)
        }
        WM_APP_TRAY => {
            match lp as u32 {
                WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
                WM_LBUTTONUP => {
                    with_state(|s| s.emit(PlatformEvent::Menu(MenuItem::OpenSala)));
                }
                NIN_BALLOONUSERCLICK => {
                    with_state(|s| s.emit(PlatformEvent::NotificationClicked));
                }
                _ => {}
            }
            Some(0)
        }
        WM_HOTKEY => {
            with_state(|s| {
                let id = wp as i32;
                if let Some((_, action)) = s.registered.iter().find(|(i, _)| *i == id) {
                    s.emit(PlatformEvent::Hotkey(*action));
                }
            });
            Some(0)
        }
        WM_TIMER if wp == POLL_TIMER => {
            with_state(|s| s.on_timer());
            Some(0)
        }
        WM_DESTROY => {
            with_state(|s| s.cleanup());
            // SAFETY: ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
            Some(0)
        }
        _ => None,
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // A panic must not unwind through the FFI boundary.
    let r = panic::catch_unwind(AssertUnwindSafe(|| handle(hwnd, msg, wp, lp)));
    match r {
        Ok(Some(v)) => v,
        // SAFETY: default processing with the arguments we were given.
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

fn create_window() -> io::Result<HWND> {
    let class = wide(CLASS_NAME);
    // SAFETY: standard class registration and hidden top-level window (not
    // message-only, so it receives the `TaskbarCreated` broadcast).
    unsafe {
        let hinstance = GetModuleHandleW(ptr::null());
        let mut wc: WNDCLASSEXW = mem::zeroed();
        wc.cbSize = mem::size_of::<WNDCLASSEXW>() as u32;
        wc.lpfnWndProc = Some(wndproc);
        wc.hInstance = hinstance;
        wc.lpszClassName = class.as_ptr();
        if RegisterClassExW(&wc) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
            return Err(io::Error::last_os_error());
        }
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            wide("Sala").as_ptr(),
            0,
            0,
            0,
            0,
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            hinstance,
            ptr::null(),
        );
        if hwnd.is_null() { Err(io::Error::last_os_error()) } else { Ok(hwnd) }
    }
}

/// Thread body. Reports the window handle (or the error) through `ready`.
pub(super) fn run(
    commands: Receiver<Command>,
    events: EventSender,
    ready: Sender<io::Result<isize>>,
) {
    let hwnd = match create_window() {
        Ok(h) => h,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    // SAFETY: registers a broadcast message name.
    let taskbar_created = unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) };
    STATE.with(|c| {
        *c.borrow_mut() = Some(State {
            hwnd,
            commands,
            events,
            tray: TrayState::default(),
            icon: ptr::null_mut(),
            icon_kind: None,
            icon_added: false,
            taskbar_created,
            shortcuts: Vec::new(),
            registered: Vec::new(),
            polled: Vec::new(),
            timer_on: false,
        });
    });
    with_state(|s| s.update_tray());
    let _ = ready.send(Ok(hwnd as isize));

    // SAFETY: standard message loop; ends on WM_QUIT (from WM_DESTROY).
    unsafe {
        let mut msg: MSG = mem::zeroed();
        while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    STATE.with(|c| *c.borrow_mut() = None);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hk(s: &str) -> Hotkey {
        s.parse().unwrap()
    }

    fn shortcuts() -> Vec<(HotkeyAction, Hotkey)> {
        vec![
            (HotkeyAction::ToggleMute, hk("Ctrl+Shift+M")),
            (HotkeyAction::ToggleDeafen, hk("Ctrl+Shift+D")),
            (HotkeyAction::ToggleGameMode, hk("Ctrl+Shift+G")),
        ]
    }

    #[test]
    fn menu_follows_design_order_and_shortcuts() {
        let tray = TrayState { game_mode: true, call: Some("Em chamada com Pedro · 18 ms".into()), ..Default::default() };
        let lines = menu_lines(&tray, &shortcuts());
        assert_eq!(
            lines,
            vec![
                MenuLine::Info("Concord · Modo jogo".into()),
                MenuLine::Info("Em chamada com Pedro · 18 ms".into()),
                MenuLine::Separator,
                MenuLine::Item(MenuItem::ToggleMute, "Mutar-se\tCtrl+Shift+M".into()),
                MenuLine::Item(MenuItem::ToggleDeafen, "Silenciar\tCtrl+Shift+D".into()),
                MenuLine::Item(MenuItem::ToggleGameMode, "Sair do modo jogo\tCtrl+Shift+G".into()),
                MenuLine::Item(MenuItem::OpenSala, "Abrir o Concord".into()),
                MenuLine::Separator,
                MenuLine::Item(MenuItem::QuitSala, "Fechar o Concord".into()),
            ]
        );
    }

    #[test]
    fn menu_labels_follow_state() {
        let tray = TrayState { muted: true, deafened: true, ..Default::default() };
        let lines = menu_lines(&tray, &[]);
        assert_eq!(lines[0], MenuLine::Info("Concord".into()));
        assert_eq!(lines[1], MenuLine::Info("Fora de chamada".into()));
        assert_eq!(lines[3], MenuLine::Item(MenuItem::ToggleMute, "Desmutar-se".into()));
        assert_eq!(lines[4], MenuLine::Item(MenuItem::ToggleDeafen, "Voltar a ouvir".into()));
        assert_eq!(lines[5], MenuLine::Item(MenuItem::ToggleGameMode, "Entrar no modo jogo".into()));
    }

    #[test]
    fn ampersand_in_call_line_is_escaped() {
        let tray = TrayState { call: Some("Em chamada com A&B".into()), ..Default::default() };
        assert_eq!(menu_lines(&tray, &[])[1], MenuLine::Info("Em chamada com A&&B".into()));
    }
}
