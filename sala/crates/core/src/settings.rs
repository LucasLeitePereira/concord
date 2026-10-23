//! Settings exposed to the UI (T-016): conversions between [`Config`] and the IPC
//! types, and the validation of `UpdateConfig`. Pure, no I/O.

use sala_protocol::{AudioSettings, CoreToUi, FriendInfo, GameModeSettings, HotkeySettings, VoiceMode};

use crate::config::{AudioConfig, AudioMode, Config, GameModeConfig, HotkeyConfig, MAX_INPUT_VOLUME_PERCENT};
use crate::voice::{MAX_BITRATE_KBPS, MIN_BITRATE_KBPS};

const MAX_NAME_CHARS: usize = 64;
/// Lowest voice-activity threshold accepted (dBFS).
pub const MIN_VAD_THRESHOLD: i32 = -100;

fn mode_to_ipc(m: AudioMode) -> VoiceMode {
    match m {
        AudioMode::VoiceActivity => VoiceMode::VoiceActivity,
        AudioMode::PushToTalk => VoiceMode::PushToTalk,
    }
}

fn mode_from_ipc(m: VoiceMode) -> AudioMode {
    match m {
        VoiceMode::VoiceActivity => AudioMode::VoiceActivity,
        VoiceMode::PushToTalk => AudioMode::PushToTalk,
    }
}

pub fn audio_settings(a: &AudioConfig) -> AudioSettings {
    AudioSettings {
        input_device: a.input_device.clone(),
        output_device: a.output_device.clone(),
        bitrate_kbps: a.bitrate_kbps,
        mode: mode_to_ipc(a.mode),
        vad_threshold: a.vad_threshold,
        input_volume: a.input_volume,
        noise_suppression: a.noise_suppression,
    }
}

/// The `Config` event for the UI: current values of everything editable.
pub fn config_message(c: &Config) -> CoreToUi {
    CoreToUi::Config {
        name: c.name.clone(),
        audio: audio_settings(&c.audio),
        game_mode: GameModeSettings {
            auto_detect: c.game_mode.auto_detect,
            games: c.game_mode.games.clone(),
            exclude: c.game_mode.exclude.clone(),
            keep_sharing_in_game_mode: c.game_mode.keep_sharing_in_game_mode,
        },
        hotkeys: HotkeySettings {
            push_to_talk: c.hotkeys.push_to_talk.clone(),
            toggle_mute: c.hotkeys.toggle_mute.clone(),
            toggle_deafen: c.hotkeys.toggle_deafen.clone(),
            toggle_game_mode: c.hotkeys.toggle_game_mode.clone(),
        },
        friends: c
            .friends
            .iter()
            .map(|f| FriendInfo {
                name: f.name.clone(),
                address: f.address.clone(),
                port: f.port,
                verified: f.fingerprint.is_some(),
            })
            .collect(),
        my_address: None,
    }
}

/// True for IPv4 in the Tailscale CGNAT range 100.64.0.0/10.
pub fn is_tailscale_ip(ip: std::net::Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 100 && (64..=127).contains(&o[1])
}

/// `ip` or `ip:port` (port only when not the default), as friends type it.
pub fn format_my_address(ip: std::net::Ipv4Addr, port: u16) -> String {
    if port == crate::config::DEFAULT_PORT { ip.to_string() } else { format!("{ip}:{port}") }
}

/// This PC's Tailscale IPv4: the source address the OS would use to reach the Tailscale
/// DNS (100.100.100.100). A UDP `connect` sends nothing.
fn detect_tailscale_ip() -> Option<std::net::Ipv4Addr> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("100.100.100.100:53").ok()?;
    match sock.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(ip) if is_tailscale_ip(ip) => Some(ip),
        _ => None,
    }
}

/// [`config_message`] plus `my_address`, which needs a (cheap) look at the network.
pub fn config_message_with_address(c: &Config) -> CoreToUi {
    let mut msg = config_message(c);
    if let CoreToUi::Config { my_address, .. } = &mut msg {
        *my_address = detect_tailscale_ip().map(|ip| format_my_address(ip, c.port));
    }
    msg
}

/// Validates the audio section. Out-of-range values are errors (not clamped), so the
/// UI learns what was wrong.
pub fn audio_from_ipc(s: &AudioSettings) -> Result<AudioConfig, String> {
    if !(MIN_BITRATE_KBPS..=MAX_BITRATE_KBPS).contains(&s.bitrate_kbps) {
        return Err(format!("bitrate_kbps must be {MIN_BITRATE_KBPS}..={MAX_BITRATE_KBPS}"));
    }
    if s.input_volume > MAX_INPUT_VOLUME_PERCENT {
        return Err(format!("input_volume must be 0..={MAX_INPUT_VOLUME_PERCENT}"));
    }
    if !(MIN_VAD_THRESHOLD..=0).contains(&s.vad_threshold) {
        return Err(format!("vad_threshold must be {MIN_VAD_THRESHOLD}..=0"));
    }
    let device = |d: &str| if d.trim().is_empty() { "default".to_string() } else { d.trim().to_string() };
    Ok(AudioConfig {
        input_device: device(&s.input_device),
        output_device: device(&s.output_device),
        bitrate_kbps: s.bitrate_kbps,
        mode: mode_from_ipc(s.mode),
        vad_threshold: s.vad_threshold,
        input_volume: s.input_volume,
        noise_suppression: s.noise_suppression,
    })
}

fn clean_list(items: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for i in items {
        let i = i.trim();
        if !i.is_empty() && !out.iter().any(|o| o.eq_ignore_ascii_case(i)) {
            out.push(i.to_string());
        }
    }
    out
}

pub fn game_mode_from_ipc(s: &GameModeSettings) -> GameModeConfig {
    GameModeConfig {
        auto_detect: s.auto_detect,
        games: clean_list(&s.games),
        exclude: clean_list(&s.exclude),
        keep_sharing_in_game_mode: s.keep_sharing_in_game_mode,
    }
}

/// Trims the strings; none may be empty. Whether they parse as hotkeys is the caller's check.
pub fn hotkeys_from_ipc(s: &HotkeySettings) -> Result<HotkeyConfig, String> {
    let one = |what: &str, v: &str| {
        let v = v.trim();
        if v.is_empty() { Err(format!("hotkey {what} is empty")) } else { Ok(v.to_string()) }
    };
    Ok(HotkeyConfig {
        push_to_talk: one("push_to_talk", &s.push_to_talk)?,
        toggle_mute: one("toggle_mute", &s.toggle_mute)?,
        toggle_deafen: one("toggle_deafen", &s.toggle_deafen)?,
        toggle_game_mode: one("toggle_game_mode", &s.toggle_game_mode)?,
    })
}

pub fn validate_name(name: &str) -> Result<String, String> {
    let n = name.trim();
    if n.is_empty() {
        return Err("name is empty".into());
    }
    if n.chars().count() > MAX_NAME_CHARS {
        return Err(format!("name is longer than {MAX_NAME_CHARS} characters"));
    }
    Ok(n.to_string())
}

/// `cur` with the present sections replaced; the first invalid section fails the whole
/// update (nothing is applied). Friends and the other sections are untouched.
pub fn merged(
    cur: &Config,
    name: Option<&str>,
    audio: Option<&AudioSettings>,
    game_mode: Option<&GameModeSettings>,
    hotkeys: Option<&HotkeySettings>,
) -> Result<Config, String> {
    let mut c = cur.clone();
    if let Some(n) = name {
        c.name = validate_name(n)?;
    }
    if let Some(a) = audio {
        c.audio = audio_from_ipc(a)?;
    }
    if let Some(g) = game_mode {
        c.game_mode = game_mode_from_ipc(g);
    }
    if let Some(h) = hotkeys {
        c.hotkeys = hotkeys_from_ipc(h)?;
    }
    Ok(c)
}

/// Device list for the UI: `"default"` first, no duplicates, empty names dropped.
pub fn with_default_first(names: Vec<String>) -> Vec<String> {
    let mut out = vec!["default".to_string()];
    for n in names {
        if !n.trim().is_empty() && !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Config {
        Config::default()
    }

    #[test]
    fn tailscale_range_is_100_64_to_100_127() {
        use std::net::Ipv4Addr as A;
        assert!(is_tailscale_ip(A::new(100, 64, 0, 1)));
        assert!(is_tailscale_ip(A::new(100, 127, 255, 254)));
        assert!(!is_tailscale_ip(A::new(100, 63, 255, 255)), "control: below range");
        assert!(!is_tailscale_ip(A::new(100, 128, 0, 1)), "control: above range");
        assert!(!is_tailscale_ip(A::new(192, 168, 0, 5)));
        assert_eq!(format_my_address(A::new(100, 64, 0, 1), crate::config::DEFAULT_PORT), "100.64.0.1");
        assert_eq!(format_my_address(A::new(100, 64, 0, 1), 47811), "100.64.0.1:47811");
    }

    #[test]
    fn config_message_mirrors_the_config_and_marks_verified() {
        let mut c = base();
        c.friends.push(crate::config::Friend {
            name: "ana".into(),
            address: "100.64.0.2".into(),
            port: Some(9),
            fingerprint: Some("ab".repeat(32)),
        });
        c.friends.push(crate::config::Friend {
            name: "bia".into(),
            address: "100.64.0.3".into(),
            port: None,
            fingerprint: None,
        });
        let CoreToUi::Config { name, audio, friends, game_mode, .. } = config_message(&c) else { panic!() };
        assert_eq!(name, c.name);
        assert_eq!(audio.bitrate_kbps, 96);
        assert_eq!(game_mode.games, c.game_mode.games);
        assert_eq!(friends.len(), 2);
        assert!(friends[0].verified && friends[0].port == Some(9));
        assert!(!friends[1].verified, "control: no pin, not verified");
    }

    #[test]
    fn merged_replaces_only_present_sections() {
        let cur = base();
        let mut a = audio_settings(&cur.audio);
        a.vad_threshold = -40;
        let c = merged(&cur, Some("  Zé "), Some(&a), None, None).unwrap();
        assert_eq!(c.name, "Zé");
        assert_eq!(c.audio.vad_threshold, -40);
        assert_eq!(c.game_mode, cur.game_mode);
        assert_eq!(c.hotkeys, cur.hotkeys);
        assert_eq!(merged(&cur, None, None, None, None).unwrap(), cur);
    }

    #[test]
    fn invalid_values_are_rejected_and_boundaries_accepted() {
        let cur = base();
        let with = |f: &dyn Fn(&mut AudioSettings)| {
            let mut a = audio_settings(&cur.audio);
            f(&mut a);
            merged(&cur, None, Some(&a), None, None)
        };
        assert!(with(&|a| a.bitrate_kbps = 31).is_err());
        assert!(with(&|a| a.bitrate_kbps = 129).is_err());
        assert!(with(&|a| a.input_volume = 201).is_err());
        assert!(with(&|a| a.vad_threshold = 1).is_err());
        assert!(with(&|a| a.vad_threshold = -101).is_err());
        assert!(merged(&cur, Some("   "), None, None, None).is_err());
        assert!(merged(&cur, Some(&"x".repeat(65)), None, None, None).is_err());
        let hk = HotkeySettings {
            push_to_talk: "Mouse4".into(),
            toggle_mute: " ".into(),
            toggle_deafen: "Ctrl+Shift+D".into(),
            toggle_game_mode: "Ctrl+Shift+G".into(),
        };
        assert!(merged(&cur, None, None, None, Some(&hk)).is_err());
        // Control: the edges themselves are valid.
        assert!(with(&|a| a.bitrate_kbps = 32).is_ok());
        assert!(with(&|a| a.bitrate_kbps = 128).is_ok());
        assert!(with(&|a| a.input_volume = 200).is_ok());
        assert!(with(&|a| a.vad_threshold = -100).is_ok());
        assert!(with(&|a| a.vad_threshold = 0).is_ok());
        assert_eq!(with(&|a| a.output_device = "  ".into()).unwrap().audio.output_device, "default");
    }

    #[test]
    fn game_lists_are_trimmed_and_deduplicated() {
        let g = GameModeSettings {
            auto_detect: false,
            games: vec![" cs2.exe ".into(), "".into(), "CS2.EXE".into(), "valorant.exe".into()],
            exclude: vec![],
            keep_sharing_in_game_mode: true,
        };
        let c = game_mode_from_ipc(&g);
        assert_eq!(c.games, ["cs2.exe", "valorant.exe"]);
        assert!(!c.auto_detect && c.keep_sharing_in_game_mode);
    }

    #[test]
    fn device_list_puts_default_first_without_duplicates() {
        let v = with_default_first(vec!["Mic".into(), "default".into(), "Mic".into(), "".into(), "Fones".into()]);
        assert_eq!(v, ["default", "Mic", "Fones"]);
        assert_eq!(with_default_first(vec![]), ["default"]);
    }
}
