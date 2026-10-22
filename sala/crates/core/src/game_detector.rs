//! Game detection: classification of one foreground-window sample and the
//! enter/exit hysteresis that drives game mode.
//!
//! API skeleton fixed by the architect. Implement the `todo!()` bodies; do not
//! change public signatures, type names, variants or public fields. Private
//! items may be added. This module is pure logic: reading the foreground window
//! with the Win32 API and acting on transitions (closing the UI, sending
//! `Status`) are separate tasks.

use std::time::Duration;

use crate::config::GameModeConfig;

/// How often the foreground window is sampled.
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// What the Win32 probe saw in one tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundSample {
    /// Full image path of the foreground window's process, as returned by
    /// `QueryFullProcessImageNameW` (e.g. `C:\Riot Games\VALORANT\VALORANT.exe`).
    /// Lists in the config hold bare file names (`valorant.exe`): matching is by
    /// file name only and case-insensitive, as Windows file names are.
    pub exe_path: String,
    /// The window rectangle covers the whole monitor it is on.
    pub covers_monitor: bool,
}

/// Game-mode state change produced by the detector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    Enter,
    Exit,
}

/// Who put the app in its current state (see `GameDetector::set_manual`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Auto,
    Manual,
}

/// Positive/negative rule for a single sample.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameRules {
    games: Vec<String>,
    exclude: Vec<String>,
}

impl GameRules {
    pub fn new(games: &[String], exclude: &[String]) -> Self {
        let _ = (games, exclude);
        todo!()
    }

    /// `None` means there is no foreground window (desktop, lock screen) and
    /// counts as negative.
    pub fn is_game(&self, sample: Option<&ForegroundSample>) -> bool {
        let _ = sample;
        todo!()
    }
}

/// Consecutive-reading hysteresis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hysteresis {
    in_game: bool,
}

impl Hysteresis {
    /// Consecutive positive readings needed to enter.
    pub const ENTER_AFTER: u32 = 2;
    /// Consecutive negative readings needed to exit.
    pub const EXIT_AFTER: u32 = 3;

    /// Starts outside game mode.
    pub fn new() -> Self {
        todo!()
    }

    pub fn in_game(&self) -> bool {
        self.in_game
    }

    /// Feeds one reading; returns the transition it caused, if any.
    pub fn observe(&mut self, positive: bool) -> Option<Transition> {
        let _ = positive;
        todo!()
    }

    /// Sets the state directly and clears the streak counters.
    pub fn reset(&mut self, in_game: bool) {
        let _ = in_game;
        todo!()
    }
}

impl Default for Hysteresis {
    fn default() -> Self {
        Self::new()
    }
}

/// Rules + hysteresis + manual override. One `tick` per `POLL_INTERVAL`.
///
/// Manual toggle (hotkey, tray, UI) versus automatic detection:
/// - Manual enter: game mode on immediately. Automatic detection never exits a
///   manually entered game mode; only a manual exit does.
/// - Manual exit: game mode off immediately. Automatic entry is suppressed until
///   at least one negative reading arrives (otherwise the still-focused game would
///   re-enter 4 s later); after that, the normal enter rule applies again,
///   counting only readings after the negative one.
/// - Automatic transitions follow the hysteresis and set the origin to `Auto`.
#[derive(Debug, Clone)]
pub struct GameDetector {
    rules: GameRules,
    hysteresis: Hysteresis,
}

impl GameDetector {
    /// Starts outside game mode. With `auto_detect = false`, `tick` never
    /// produces a transition (manual toggles still work).
    pub fn new(cfg: &GameModeConfig) -> Self {
        let _ = cfg;
        todo!()
    }

    pub fn in_game(&self) -> bool {
        self.hysteresis.in_game()
    }

    /// Origin of the current state; `Auto` before any transition.
    pub fn origin(&self) -> Origin {
        todo!()
    }

    /// One sampling tick.
    pub fn tick(&mut self, sample: Option<&ForegroundSample>) -> Option<Transition> {
        let _ = sample;
        todo!()
    }

    /// Manual toggle. Returns the transition, or `None` if already in that state
    /// (the origin still becomes `Manual`).
    pub fn set_manual(&mut self, in_game: bool) -> Option<Transition> {
        let _ = in_game;
        todo!()
    }

    /// Applies a reloaded config (lists and `auto_detect`). Keeps the current
    /// state and origin; clears the streak counters.
    pub fn set_config(&mut self, cfg: &GameModeConfig) {
        let _ = cfg;
        todo!()
    }
}
