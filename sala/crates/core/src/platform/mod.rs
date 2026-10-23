//! Win32 layer of the core process (D-009): tray icon, global hotkeys,
//! notification and foreground-window probe. One platform thread owns a hidden
//! window and a message loop; other threads talk to it through [`Platform`]
//! (commands) and the [`PlatformEvent`] channel returned by [`Platform::start`].
//!
//! No hooks and no Raw Input: keyboard shortcuts use `RegisterHotKey`, mouse
//! buttons and push-to-talk poll `GetAsyncKeyState` every 15 ms, and only while
//! something needs it.

pub mod foreground;
pub mod hotkey;
mod icon;
mod window;

use std::io;
use std::sync::Mutex;
use std::sync::mpsc;
use std::thread::JoinHandle;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

pub use foreground::probe;
pub use hotkey::{Hotkey, HotkeyKey, Modifiers, MouseButton, ParseHotkeyError};

/// Readable name for logs (same words as the `[hotkeys]` config keys).
impl std::fmt::Display for HotkeyAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HotkeyAction::ToggleMute => f.write_str("toggle_mute"),
            HotkeyAction::ToggleDeafen => f.write_str("toggle_deafen"),
            HotkeyAction::ToggleGameMode => f.write_str("toggle_game_mode"),
            HotkeyAction::PlaySound(i) => write!(f, "play_sound[{i}]"),
        }
    }
}

/// What a global shortcut does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HotkeyAction {
    ToggleMute,
    ToggleDeafen,
    ToggleGameMode,
    /// Soundboard entry, index in `Config::soundboard`.
    PlaySound(usize),
}

/// Tray menu entries (order in `ModoJogo.dc.html`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuItem {
    ToggleMute,
    ToggleDeafen,
    ToggleGameMode,
    /// Also sent by a left click on the tray icon.
    OpenSala,
    QuitSala,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlatformEvent {
    Hotkey(HotkeyAction),
    PttPressed,
    PttReleased,
    Menu(MenuItem),
    /// The user clicked the chat notification ("Abrir o chat").
    NotificationClicked,
    /// `RegisterHotKey` refused the combination (usually taken by another app).
    HotkeyRegistrationFailed {
        action: HotkeyAction,
        hotkey: Hotkey,
        error_code: u32,
    },
}

/// What the tray icon and menu show.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrayState {
    pub game_mode: bool,
    pub muted: bool,
    pub deafened: bool,
    /// Call line of the menu, e.g. `"Em chamada com Pedro · 18 ms"`; `None` = no call.
    pub call: Option<String>,
}

pub(crate) enum Command {
    Tray(TrayState),
    Notify { sender: String },
    Hotkeys(Vec<(HotkeyAction, Hotkey)>),
    Ptt(Option<Hotkey>),
    Shutdown,
}

/// Handle to the platform thread. Dropping it shuts the thread down.
pub struct Platform {
    hwnd: isize,
    commands: mpsc::Sender<Command>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Platform {
    /// Starts the platform thread, adds the tray icon and returns once the
    /// window exists (or the reason it could not be created).
    pub fn start() -> io::Result<(Platform, UnboundedReceiver<PlatformEvent>)> {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (ev_tx, ev_rx) = unbounded_channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("sala-platform".into())
            .spawn(move || window::run(cmd_rx, ev_tx, ready_tx))?;
        match ready_rx.recv() {
            Ok(Ok(hwnd)) => Ok((
                Platform { hwnd, commands: cmd_tx, thread: Mutex::new(Some(thread)) },
                ev_rx,
            )),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err(io::Error::other("platform thread died during start-up"))
            }
        }
    }

    fn send(&self, cmd: Command) {
        if self.commands.send(cmd).is_ok() {
            // SAFETY: posting to a window handle is sound even if it is gone
            // (the call just fails).
            unsafe {
                PostMessageW(self.hwnd as HWND, window::WM_APP_COMMAND, 0, 0);
            }
        }
    }

    /// Updates icon, tooltip and menu texts.
    pub fn set_tray(&self, state: TrayState) {
        self.send(Command::Tray(state));
    }

    /// Chat notification in game mode: sender name only, never the message
    /// text (D-011). Clicking it yields [`PlatformEvent::NotificationClicked`].
    pub fn notify_message(&self, sender: &str) {
        self.send(Command::Notify { sender: sender.to_string() });
    }

    /// Replaces the whole list of global shortcuts. Combinations that cannot
    /// be registered come back as [`PlatformEvent::HotkeyRegistrationFailed`].
    pub fn set_hotkeys(&self, hotkeys: Vec<(HotkeyAction, Hotkey)>) {
        self.send(Command::Hotkeys(hotkeys));
    }

    /// `Some` turns PTT polling on for that key/button; `None` turns it off
    /// (a held PTT is released first).
    pub fn set_ptt(&self, hotkey: Option<Hotkey>) {
        self.send(Command::Ptt(hotkey));
    }

    /// Removes the icon, unregisters shortcuts and joins the thread.
    pub fn shutdown(&self) {
        self.send(Command::Shutdown);
        let handle = self.thread.lock().ok().and_then(|mut t| t.take());
        if let Some(h) = handle {
            let _ = h.join();
        }
    }
}

impl Drop for Platform {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub(crate) type EventSender = UnboundedSender<PlatformEvent>;

/// UTF-16, NUL-terminated.
pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Copies `s` into a fixed buffer, truncating and NUL-terminating.
pub(crate) fn copy_wide(dst: &mut [u16], s: &str) {
    let mut n = 0;
    for u in s.encode_utf16().take(dst.len().saturating_sub(1)) {
        dst[n] = u;
        n += 1;
    }
    dst[n] = 0;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn copy_wide_truncates_and_terminates() {
        let mut b = [0xFFFFu16; 4];
        copy_wide(&mut b, "abcdef");
        assert_eq!(b, [97, 98, 99, 0]);
        copy_wide(&mut b, "a");
        assert_eq!(&b[..2], &[97, 0]);
    }

    /// Window and tray icon are created on a hidden window, so this works in a
    /// session without a visible desktop foreground. A failure here means the
    /// session has no interactive desktop (e.g. a service account).
    #[test]
    fn thread_starts_registers_hotkey_and_shuts_down() {
        let (p, mut events) = Platform::start().expect("platform thread starts");
        p.set_tray(TrayState { game_mode: true, call: Some("Em chamada com A&B".into()), ..Default::default() });
        // Improbable combination: must register without an error event.
        let improbable: Hotkey = "Ctrl+Alt+Shift+F24".parse().unwrap();
        p.set_hotkeys(vec![(HotkeyAction::ToggleGameMode, improbable)]);
        p.set_ptt(Some("Mouse4".parse().unwrap()));
        p.set_ptt(None);
        p.set_hotkeys(vec![]);
        p.shutdown();
        // Channel closes once the thread is gone; no failure events were sent.
        let mut got = Vec::new();
        while let Ok(ev) = events.try_recv() {
            got.push(ev);
        }
        assert!(got.is_empty(), "unexpected events: {got:?}");
        // second shutdown (and Drop) is a no-op
        p.shutdown();
    }

    /// Controle negativo: registering the same combination twice (two
    /// platform threads) must report the refusal as an event. The two threads
    /// register concurrently, so whichever loses the race reports it.
    #[test]
    fn duplicate_registration_reports_failure() {
        let hk: Hotkey = "Ctrl+Alt+Shift+F23".parse().unwrap();
        let (a, mut ea) = Platform::start().unwrap();
        let (b, mut eb) = Platform::start().unwrap();
        a.set_hotkeys(vec![(HotkeyAction::ToggleMute, hk)]);
        b.set_hotkeys(vec![(HotkeyAction::ToggleDeafen, hk)]);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let ev = loop {
            if let Ok(ev) = ea.try_recv() {
                break ev;
            }
            if let Ok(ev) = eb.try_recv() {
                break ev;
            }
            assert!(std::time::Instant::now() < deadline, "no failure event");
            std::thread::sleep(Duration::from_millis(20));
        };
        let PlatformEvent::HotkeyRegistrationFailed { action, hotkey, error_code } = ev else {
            panic!("unexpected event: {ev:?}");
        };
        assert!(matches!(action, HotkeyAction::ToggleMute | HotkeyAction::ToggleDeafen));
        assert_eq!(hotkey, hk);
        assert_eq!(error_code, 1409); // ERROR_HOTKEY_ALREADY_REGISTERED
        std::thread::sleep(Duration::from_millis(100));
        assert!(ea.try_recv().is_err() && eb.try_recv().is_err(), "only the loser reports");
        drop(a);
        drop(b);
    }
}
