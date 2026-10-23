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

/// The app's own executables (lower case): never classified as a game.
pub const OWN_EXES: [&str; 4] = ["sala-core.exe", "sala-ui.exe", "concord.exe", "concord-ui.exe"];

/// Positive/negative rule for a single sample.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameRules {
    games: Vec<String>,
    exclude: Vec<String>,
}

/// Lower-cased, trimmed file name of a path (separators `\` and `/`).
fn file_name_key(path: &str) -> String {
    let name = path.rsplit(['\\', '/']).next().unwrap_or("");
    name.trim().to_lowercase()
}

fn normalize_list(list: &[String]) -> Vec<String> {
    list.iter()
        .map(|s| file_name_key(s))
        .filter(|s| !s.is_empty())
        .collect()
}

impl GameRules {
    pub fn new(games: &[String], exclude: &[String]) -> Self {
        GameRules {
            games: normalize_list(games),
            exclude: normalize_list(exclude),
        }
    }

    /// `None` means there is no foreground window (desktop, lock screen) and
    /// counts as negative. The app's own executables ([`OWN_EXES`]) are never a
    /// game, whatever the config says (even if listed in `games`): otherwise the
    /// maximized UI window would close itself.
    pub fn is_game(&self, sample: Option<&ForegroundSample>) -> bool {
        let Some(sample) = sample else {
            return false;
        };
        let name = file_name_key(&sample.exe_path);
        if OWN_EXES.contains(&name.as_str()) {
            return false;
        }
        if !name.is_empty() && self.games.contains(&name) {
            return true;
        }
        sample.covers_monitor && (name.is_empty() || !self.exclude.contains(&name))
    }
}

/// Consecutive-reading hysteresis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hysteresis {
    in_game: bool,
    positives: u32,
    negatives: u32,
}

impl Hysteresis {
    /// Consecutive positive readings needed to enter.
    pub const ENTER_AFTER: u32 = 2;
    /// Consecutive negative readings needed to exit.
    pub const EXIT_AFTER: u32 = 3;

    /// Starts outside game mode.
    pub fn new() -> Self {
        Hysteresis {
            in_game: false,
            positives: 0,
            negatives: 0,
        }
    }

    pub fn in_game(&self) -> bool {
        self.in_game
    }

    /// Feeds one reading; returns the transition it caused, if any.
    pub fn observe(&mut self, positive: bool) -> Option<Transition> {
        if self.in_game {
            self.positives = 0;
            if positive {
                self.negatives = 0;
                return None;
            }
            self.negatives += 1;
            if self.negatives >= Self::EXIT_AFTER {
                self.reset(false);
                return Some(Transition::Exit);
            }
        } else {
            self.negatives = 0;
            if !positive {
                self.positives = 0;
                return None;
            }
            self.positives += 1;
            if self.positives >= Self::ENTER_AFTER {
                self.reset(true);
                return Some(Transition::Enter);
            }
        }
        None
    }

    /// Sets the state directly and clears the streak counters.
    pub fn reset(&mut self, in_game: bool) {
        self.in_game = in_game;
        self.positives = 0;
        self.negatives = 0;
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
    auto_detect: bool,
    origin: Origin,
    /// After a manual exit: ignore positive readings until a negative one.
    suppress_enter: bool,
}

impl GameDetector {
    /// Starts outside game mode. With `auto_detect = false`, `tick` never
    /// produces a transition (manual toggles still work).
    pub fn new(cfg: &GameModeConfig) -> Self {
        GameDetector {
            rules: GameRules::new(&cfg.games, &cfg.exclude),
            hysteresis: Hysteresis::new(),
            auto_detect: cfg.auto_detect,
            origin: Origin::Auto,
            suppress_enter: false,
        }
    }

    pub fn in_game(&self) -> bool {
        self.hysteresis.in_game()
    }

    /// Origin of the current state; `Auto` before any transition.
    pub fn origin(&self) -> Origin {
        self.origin
    }

    /// One sampling tick.
    pub fn tick(&mut self, sample: Option<&ForegroundSample>) -> Option<Transition> {
        if !self.auto_detect {
            return None;
        }
        // Automatic detection never exits a manually entered game mode.
        if self.in_game() && self.origin == Origin::Manual {
            return None;
        }
        let positive = self.rules.is_game(sample);
        if self.suppress_enter {
            if positive {
                return None;
            }
            self.suppress_enter = false;
        }
        let t = self.hysteresis.observe(positive);
        if t.is_some() {
            self.origin = Origin::Auto;
        }
        t
    }

    /// Manual toggle. Returns the transition, or `None` if already in that state
    /// (the origin still becomes `Manual`).
    pub fn set_manual(&mut self, in_game: bool) -> Option<Transition> {
        let was = self.hysteresis.in_game();
        self.hysteresis.reset(in_game);
        self.origin = Origin::Manual;
        self.suppress_enter = !in_game;
        match (was, in_game) {
            (false, true) => Some(Transition::Enter),
            (true, false) => Some(Transition::Exit),
            _ => None,
        }
    }

    /// Applies a reloaded config (lists and `auto_detect`). Keeps the current
    /// state and origin; clears the streak counters.
    pub fn set_config(&mut self, cfg: &GameModeConfig) {
        self.rules = GameRules::new(&cfg.games, &cfg.exclude);
        self.auto_detect = cfg.auto_detect;
        let cur = self.hysteresis.in_game();
        self.hysteresis.reset(cur);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(path: &str, full: bool) -> ForegroundSample {
        ForegroundSample {
            exe_path: path.into(),
            covers_monitor: full,
        }
    }
    fn game() -> ForegroundSample {
        s("C:\\Riot Games\\VALORANT\\VALORANT.exe", false)
    }
    fn desk() -> ForegroundSample {
        s("C:\\Windows\\explorer.exe", true)
    }
    fn det() -> GameDetector {
        GameDetector::new(&GameModeConfig::default())
    }

    #[test]
    fn rules_listed_game_case_insensitive_by_file_name() {
        let r = GameRules::new(&["valorant.exe".into()], &[]);
        assert!(r.is_game(Some(&game())));
        assert!(r.is_game(Some(&s("d:/x/Valorant.EXE", false))));
        assert!(!r.is_game(Some(&s("C:\\valorant.exe.bak", false))));
        assert!(!r.is_game(Some(&s("C:\\notvalorant.exe", false))));
    }

    #[test]
    fn rules_fullscreen_unless_excluded() {
        let r = GameRules::new(&[], &["Chrome.exe".into(), "explorer.exe".into()]);
        assert!(r.is_game(Some(&s("C:\\foo\\unknown.exe", true))));
        assert!(!r.is_game(Some(&s("C:\\foo\\unknown.exe", false))));
        assert!(!r.is_game(Some(&s("C:\\Program Files\\chrome.exe", true))));
        assert!(!r.is_game(Some(&desk())));
    }

    #[test]
    fn rules_never_treat_own_exes_as_game() {
        // Even listed in `games` and covering the monitor.
        let r = GameRules::new(&["sala-ui.exe".into(), "sala-core.exe".into(), "Concord.exe".into(), "concord-ui.exe".into()], &[]);
        for p in ["C:\\Concord\\sala-ui.exe", "C:\\x\\SALA-CORE.EXE", "D:/c/concord.exe", "concord-ui.exe"] {
            assert!(!r.is_game(Some(&s(p, true))), "{p}");
            assert!(!r.is_game(Some(&s(p, false))), "{p}");
        }
        // Control: a similarly named foreign exe in full screen is still a game.
        assert!(r.is_game(Some(&s("C:\\x\\sala-ui2.exe", true))));
    }

    #[test]
    fn rules_listed_game_wins_over_exclude_and_none_is_negative() {
        let r = GameRules::new(&["a.exe".into()], &["a.exe".into()]);
        assert!(r.is_game(Some(&s("a.exe", false))));
        assert!(!r.is_game(None));
    }

    #[test]
    fn hysteresis_enter_after_two_exit_after_three() {
        let mut h = Hysteresis::new();
        assert_eq!(h.observe(true), None);
        assert_eq!(h.observe(true), Some(Transition::Enter));
        assert!(h.in_game());
        assert_eq!(h.observe(false), None);
        assert_eq!(h.observe(false), None);
        assert_eq!(h.observe(false), Some(Transition::Exit));
        assert!(!h.in_game());
    }

    #[test]
    fn hysteresis_streaks_must_be_consecutive() {
        let mut h = Hysteresis::new();
        for _ in 0..5 {
            assert_eq!(h.observe(true), None);
            assert_eq!(h.observe(false), None);
        }
        assert!(!h.in_game());
        h.reset(true);
        for _ in 0..5 {
            h.observe(false);
            h.observe(false);
            assert_eq!(h.observe(true), None);
            assert!(h.in_game());
        }
    }

    #[test]
    fn hysteresis_reset_clears_streaks() {
        let mut h = Hysteresis::new();
        h.observe(true);
        h.reset(false);
        assert_eq!(h.observe(true), None);
        h.reset(true);
        h.observe(false);
        h.observe(false);
        h.reset(true);
        assert_eq!(h.observe(false), None);
        assert!(h.in_game());
    }

    #[test]
    fn detector_auto_enter_exit_sets_origin_auto() {
        let mut d = det();
        assert_eq!(d.origin(), Origin::Auto);
        assert_eq!(d.tick(Some(&game())), None);
        assert_eq!(d.tick(Some(&game())), Some(Transition::Enter));
        assert!(d.in_game());
        assert_eq!(d.tick(None), None);
        assert_eq!(d.tick(Some(&desk())), None);
        assert_eq!(d.tick(None), Some(Transition::Exit));
        assert_eq!(d.origin(), Origin::Auto);
    }

    #[test]
    fn auto_detect_off_never_transitions_but_manual_works() {
        let cfg = GameModeConfig {
            auto_detect: false,
            ..GameModeConfig::default()
        };
        let mut d = GameDetector::new(&cfg);
        for _ in 0..5 {
            assert_eq!(d.tick(Some(&game())), None);
        }
        assert_eq!(d.set_manual(true), Some(Transition::Enter));
        for _ in 0..5 {
            assert_eq!(d.tick(None), None);
        }
        assert!(d.in_game());
    }

    #[test]
    fn manual_enter_is_not_exited_by_auto() {
        let mut d = det();
        assert_eq!(d.set_manual(true), Some(Transition::Enter));
        assert_eq!(d.origin(), Origin::Manual);
        for _ in 0..10 {
            assert_eq!(d.tick(None), None);
        }
        assert!(d.in_game());
        assert_eq!(d.set_manual(false), Some(Transition::Exit));
        assert!(!d.in_game());
    }

    #[test]
    fn manual_noop_changes_origin_only() {
        let mut d = det();
        assert_eq!(d.set_manual(false), None);
        assert_eq!(d.origin(), Origin::Manual);
        let mut d = det();
        d.tick(Some(&game()));
        d.tick(Some(&game()));
        assert_eq!(d.set_manual(true), None);
        assert_eq!(d.origin(), Origin::Manual);
        assert_eq!(d.tick(None), None);
        assert!(d.in_game());
    }

    #[test]
    fn manual_exit_suppresses_auto_entry_until_negative() {
        let mut d = det();
        d.tick(Some(&game()));
        d.tick(Some(&game()));
        assert_eq!(d.set_manual(false), Some(Transition::Exit));
        for _ in 0..10 {
            assert_eq!(d.tick(Some(&game())), None);
        }
        assert!(!d.in_game());
        // one negative lifts the suppression; positives before it do not count
        assert_eq!(d.tick(None), None);
        assert_eq!(d.tick(Some(&game())), None);
        assert_eq!(d.tick(Some(&game())), Some(Transition::Enter));
        assert_eq!(d.origin(), Origin::Auto);
    }

    #[test]
    fn auto_exit_works_after_manual_cycle() {
        let mut d = det();
        d.set_manual(true);
        d.set_manual(false);
        d.tick(None);
        d.tick(Some(&game()));
        assert_eq!(d.tick(Some(&game())), Some(Transition::Enter));
        d.tick(None);
        d.tick(None);
        assert_eq!(d.tick(None), Some(Transition::Exit));
    }

    #[test]
    fn set_config_keeps_state_and_origin_clears_streaks() {
        let mut d = det();
        d.tick(Some(&game()));
        d.set_config(&GameModeConfig::default());
        // streak was cleared: one more positive is not enough
        assert_eq!(d.tick(Some(&game())), None);
        assert_eq!(d.tick(Some(&game())), Some(Transition::Enter));
        d.tick(None);
        d.tick(None);
        d.set_config(&GameModeConfig::default());
        assert_eq!(d.tick(None), None);
        assert!(d.in_game());
        assert_eq!(d.origin(), Origin::Auto);
        d.set_manual(true);
        d.set_config(&GameModeConfig::default());
        assert_eq!(d.origin(), Origin::Manual);
    }

    #[test]
    fn set_config_applies_new_lists_and_auto_flag() {
        let mut d = det();
        let other = s("C:\\g\\mygame.exe", false);
        d.tick(Some(&other));
        assert_eq!(d.tick(Some(&other)), None);
        let cfg = GameModeConfig {
            games: vec!["MyGame.exe".into()],
            ..GameModeConfig::default()
        };
        d.set_config(&cfg);
        d.tick(Some(&other));
        assert_eq!(d.tick(Some(&other)), Some(Transition::Enter));
        let off = GameModeConfig {
            auto_detect: false,
            ..cfg
        };
        d.set_config(&off);
        for _ in 0..5 {
            assert_eq!(d.tick(None), None);
        }
    }
}
