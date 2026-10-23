//! Persisted configuration (`<data_dir>/config.toml`) and instance profiles (D-005).

use std::fs;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Default UDP port of the default instance.
pub const DEFAULT_PORT: u16 = 47800;
const MIN_BITRATE_KBPS: u32 = 32;
const MAX_BITRATE_KBPS: u32 = 128;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("invalid profile name {0:?}: use only [a-z0-9-] (at least one character)")]
    InvalidProfileName(String),
    #[error("cannot locate the data root: neither SALA_APPDATA nor APPDATA is set")]
    NoDataRoot,
    #[error("friend {friend:?} has an invalid address {address:?}: expected an IP such as 100.64.0.1")]
    InvalidAddress { friend: String, address: String },
    #[error("I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot parse {path}: {source}")]
    Parse {
        path: PathBuf,
        source: Box<toml::de::Error>,
    },
    #[error("cannot serialize config: {0}")]
    Serialize(#[from] Box<toml::ser::Error>),
}

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> ConfigError + '_ {
    move |source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Instance isolation: data directory and default port (D-005).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    /// `None` = default profile (`%APPDATA%\Sala`).
    pub name: Option<String>,
    pub data_dir: PathBuf,
    pub port: u16,
}

impl Profile {
    /// Resolves using `SALA_APPDATA` (if set) or `APPDATA` as the root.
    pub fn resolve(name: Option<&str>) -> Result<Profile, ConfigError> {
        let root = std::env::var_os("SALA_APPDATA")
            .filter(|v| !v.is_empty())
            .or_else(|| std::env::var_os("APPDATA").filter(|v| !v.is_empty()))
            .ok_or(ConfigError::NoDataRoot)?;
        Profile::resolve_in(Path::new(&root), name)
    }

    /// Like [`Profile::resolve`] but with an explicit root (stands in for `%APPDATA%`).
    pub fn resolve_in(root: &Path, name: Option<&str>) -> Result<Profile, ConfigError> {
        let base = root.join("Sala");
        match name {
            None => Ok(Profile {
                name: None,
                data_dir: base,
                port: DEFAULT_PORT,
            }),
            Some(n) => {
                let valid = !n.is_empty()
                    && n.chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
                if !valid {
                    return Err(ConfigError::InvalidProfileName(n.to_string()));
                }
                let port = match n {
                    "a" => 47801,
                    "b" => 47802,
                    _ => 47803,
                };
                Ok(Profile {
                    name: Some(n.to_string()),
                    data_dir: base.join("profiles").join(n),
                    port,
                })
            }
        }
    }

    pub fn config_path(&self) -> PathBuf {
        self.data_dir.join("config.toml")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Friend {
    pub name: String,
    /// IP address (Tailscale 100.x.y.z; 127.0.0.1 in tests). Validated on load.
    pub address: String,
    /// UDP port; `None` = 47800.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// SHA-256 of the friend's certificate, lowercase hex; filled on first connection (TOFU).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
}

impl Friend {
    pub fn effective_port(&self) -> u16 {
        self.port.unwrap_or(DEFAULT_PORT)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioMode {
    VoiceActivity,
    PushToTalk,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    pub input_device: String,
    pub output_device: String,
    pub bitrate_kbps: u32,
    pub mode: AudioMode,
    pub vad_threshold: i32,
    pub noise_suppression: bool,
}

impl Default for AudioConfig {
    fn default() -> Self {
        AudioConfig {
            input_device: "default".into(),
            output_device: "default".into(),
            bitrate_kbps: 96,
            mode: AudioMode::VoiceActivity,
            vad_threshold: -45,
            noise_suppression: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeyConfig {
    pub push_to_talk: String,
    pub toggle_mute: String,
    pub toggle_deafen: String,
    pub toggle_game_mode: String,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        HotkeyConfig {
            push_to_talk: "Mouse4".into(),
            toggle_mute: "Ctrl+Shift+M".into(),
            toggle_deafen: "Ctrl+Shift+D".into(),
            toggle_game_mode: "Ctrl+Shift+G".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GameModeConfig {
    pub auto_detect: bool,
    pub games: Vec<String>,
    pub exclude: Vec<String>,
    pub keep_sharing_in_game_mode: bool,
}

impl Default for GameModeConfig {
    fn default() -> Self {
        GameModeConfig {
            auto_detect: true,
            games: vec!["valorant.exe".into(), "cs2.exe".into()],
            exclude: ["explorer.exe", "chrome.exe", "msedge.exe", "firefox.exe", "vlc.exe", "mpv.exe"]
                .into_iter()
                .map(String::from)
                .collect(),
            keep_sharing_in_game_mode: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShareConfig {
    /// `leve`, `padrao`, `alta` or `maxima`.
    pub preset: String,
    pub share_audio: bool,
}

impl Default for ShareConfig {
    fn default() -> Self {
        ShareConfig {
            preset: "padrao".into(),
            share_audio: true,
        }
    }
}

fn default_volume() -> f32 {
    1.0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SoundConfig {
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hotkey: Option<String>,
    #[serde(default = "default_volume")]
    pub volume: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub name: String,
    pub port: u16,
    pub friends: Vec<Friend>,
    pub audio: AudioConfig,
    pub hotkeys: HotkeyConfig,
    pub game_mode: GameModeConfig,
    pub share: ShareConfig,
    pub soundboard: Vec<SoundConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            name: "Seu nome".into(),
            port: DEFAULT_PORT,
            friends: Vec::new(),
            audio: AudioConfig::default(),
            hotkeys: HotkeyConfig::default(),
            game_mode: GameModeConfig::default(),
            share: ShareConfig::default(),
            soundboard: Vec::new(),
        }
    }
}

/// TOML basic string with proper escaping.
fn q(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

fn qlist(items: &[String]) -> String {
    let v: Vec<String> = items.iter().map(|s| q(s)).collect();
    format!("[{}]", v.join(", "))
}

impl Config {
    /// First-run defaults for a profile (D-005): port, name and the crossed friend for `a`/`b`.
    pub fn default_for(profile: &Profile) -> Config {
        let mut c = Config {
            port: profile.port,
            ..Config::default()
        };
        if let Some(n) = &profile.name {
            c.name = n.clone();
        }
        let cross = match profile.name.as_deref() {
            Some("a") => Some(("b", 47802)),
            Some("b") => Some(("a", 47801)),
            _ => None,
        };
        if let Some((name, port)) = cross {
            c.friends.push(Friend {
                name: name.into(),
                address: "127.0.0.1".into(),
                port: Some(port),
                fingerprint: None,
            });
        }
        c
    }

    /// Checks and normalizes: clamps `bitrate_kbps` to 32..=128 (with a warning) and
    /// rejects friends whose address is not an IP.
    pub fn validate(&mut self) -> Result<(), ConfigError> {
        let b = self.audio.bitrate_kbps;
        let clamped = b.clamp(MIN_BITRATE_KBPS, MAX_BITRATE_KBPS);
        if clamped != b {
            tracing::warn!(
                "audio.bitrate_kbps = {b} is outside {MIN_BITRATE_KBPS}..={MAX_BITRATE_KBPS}; using {clamped}"
            );
            self.audio.bitrate_kbps = clamped;
        }
        for f in &self.friends {
            if f.address.parse::<IpAddr>().is_err() {
                return Err(ConfigError::InvalidAddress {
                    friend: f.name.clone(),
                    address: f.address.clone(),
                });
            }
        }
        Ok(())
    }

    /// Parses TOML text (missing fields take defaults) and validates.
    pub fn from_toml_str(text: &str, path: &Path) -> Result<Config, ConfigError> {
        let mut c: Config = toml::from_str(text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source: Box::new(source),
        })?;
        c.validate()?;
        Ok(c)
    }

    /// Reads `<data_dir>/config.toml`, creating it with commented defaults on first run.
    /// An existing file is never overwritten.
    pub fn load_or_create(profile: &Profile) -> Result<Config, ConfigError> {
        let path = profile.config_path();
        if path.exists() {
            let text = fs::read_to_string(&path).map_err(io_err(&path))?;
            return Config::from_toml_str(&text, &path);
        }
        let config = Config::default_for(profile);
        fs::create_dir_all(&profile.data_dir).map_err(io_err(&profile.data_dir))?;
        fs::write(&path, config.to_commented_toml()).map_err(io_err(&path))?;
        Ok(config)
    }

    /// Rewrites the config file. Comments from the first-run template are NOT preserved
    /// (plain serialization), which is accepted.
    pub fn save(&self, profile: &Profile) -> Result<(), ConfigError> {
        let text = toml::to_string_pretty(self).map_err(|e| ConfigError::Serialize(Box::new(e)))?;
        fs::create_dir_all(&profile.data_dir).map_err(io_err(&profile.data_dir))?;
        let path = profile.config_path();
        fs::write(&path, text).map_err(io_err(&path))
    }

    /// Renders the first-run `config.toml`: current values plus pt-BR comments.
    /// Parsing the result yields an equal `Config`.
    pub fn to_commented_toml(&self) -> String {
        let mut s = String::new();
        s.push_str("# Configuração do Sala. Apague uma linha para voltar ao valor padrão.\n\n");
        s.push_str(&format!("name = {}\n", q(&self.name)));
        s.push_str(&format!("port = {}   # porta UDP em que você escuta\n\n", self.port));

        s.push_str("# Amigos: só quem estiver aqui pode conectar. Adicione um bloco [[friends]] por amigo.\n");
        for f in &self.friends {
            s.push_str("[[friends]]\n");
            s.push_str(&format!("name = {}\n", q(&f.name)));
            s.push_str(&format!("address = {}\n", q(&f.address)));
            if let Some(p) = f.port {
                s.push_str(&format!("port = {p}   # porta do amigo (padrão {DEFAULT_PORT})\n"));
            }
            match &f.fingerprint {
                Some(fp) => s.push_str(&format!("fingerprint = {}\n", q(fp))),
                None => s.push_str("# fingerprint é preenchido automaticamente na primeira conexão\n"),
            }
            s.push('\n');
        }
        if self.friends.is_empty() {
            s.push_str("# [[friends]]\n# name = \"Amigo\"\n# address = \"100.x.y.z\"   # IP do Tailscale\n\n");
        }

        let a = &self.audio;
        s.push_str("[audio]\n");
        s.push_str(&format!("input_device = {}\n", q(&a.input_device)));
        s.push_str(&format!("output_device = {}\n", q(&a.output_device)));
        s.push_str(&format!("bitrate_kbps = {}   # de 32 a 128\n", a.bitrate_kbps));
        let mode = match a.mode {
            AudioMode::VoiceActivity => "voice_activity",
            AudioMode::PushToTalk => "push_to_talk",
        };
        s.push_str(&format!("mode = {}   # ou \"push_to_talk\"\n", q(mode)));
        s.push_str(&format!("vad_threshold = {}   # dB\n", a.vad_threshold));
        s.push_str(&format!("noise_suppression = {}\n\n", a.noise_suppression));

        let h = &self.hotkeys;
        s.push_str("[hotkeys]\n");
        s.push_str(&format!("push_to_talk = {}\n", q(&h.push_to_talk)));
        s.push_str(&format!("toggle_mute = {}\n", q(&h.toggle_mute)));
        s.push_str(&format!("toggle_deafen = {}\n", q(&h.toggle_deafen)));
        s.push_str(&format!("toggle_game_mode ={}\n\n", q(&h.toggle_game_mode)));

        let g = &self.game_mode;
        s.push_str("[game_mode]\n");
        s.push_str(&format!("auto_detect = {}\n", g.auto_detect));
        s.push_str(&format!("games = {}\n", qlist(&g.games)));
        s.push_str(&format!("exclude = {}\n", qlist(&g.exclude)));
        s.push_str(&format!(
            "keep_sharing_in_game_mode = {}\n\n",
            g.keep_sharing_in_game_mode
        ));

        s.push_str("[share]\n");
        s.push_str(&format!(
            "preset = {}   # leve, padrao, alta ou maxima\n",
            q(&self.share.preset)
        ));
        s.push_str(&format!("share_audio = {}\n\n", self.share.share_audio));

        s.push_str("# Mesa de sons: arquivos na pasta \"sons\" do diretório de dados.\n");
        for snd in &self.soundboard {
            s.push_str("[[soundboard]]\n");
            s.push_str(&format!("file = {}\n", q(&snd.file)));
            if let Some(hk) = &snd.hotkey {
                s.push_str(&format!("hotkey = {}\n", q(hk)));
            }
            s.push_str(&format!("volume = {:?}\n\n", snd.volume));
        }
        if self.soundboard.is_empty() {
            s.push_str("# [[soundboard]]\n# file = \"exemplo.ogg\"\n# hotkey = \"Ctrl+Numpad1\"\n# volume = 1.0\n");
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fresh temp root per test (never the real %APPDATA%).
    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sala-test-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn p(root: &Path, name: Option<&str>) -> Profile {
        Profile::resolve_in(root, name).unwrap()
    }

    #[test]
    fn default_profile_paths_and_port() {
        let root = temp_root("defprof");
        let pr = p(&root, None);
        assert_eq!(pr.data_dir, root.join("Sala"));
        assert_eq!(pr.port, 47800);
        assert_eq!(pr.name, None);
    }

    #[test]
    fn profiles_a_b_other_are_distinct() {
        let root = temp_root("profs");
        let (a, b, c) = (p(&root, Some("a")), p(&root, Some("b")), p(&root, Some("xyz-1")));
        assert_eq!((a.port, b.port, c.port), (47801, 47802, 47803));
        assert_ne!(a.data_dir, b.data_dir);
        assert_eq!(a.data_dir, root.join("Sala").join("profiles").join("a"));
    }

    #[test]
    fn invalid_profile_names_rejected() {
        let root = temp_root("badname");
        for bad in ["", "A", "a b", "../x", "a/b", "é"] {
            assert!(
                matches!(Profile::resolve_in(&root, Some(bad)), Err(ConfigError::InvalidProfileName(_))),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn first_run_creates_commented_file_that_round_trips() {
        let root = temp_root("roundtrip");
        for name in [None, Some("a"), Some("b"), Some("zz")] {
            let pr = p(&root, name);
            assert!(!pr.config_path().exists());
            let created = Config::load_or_create(&pr).unwrap();
            let text = fs::read_to_string(pr.config_path()).unwrap();
            assert!(text.contains("# "), "template must carry comments");
            assert_eq!(created, Config::default_for(&pr));
            let parsed = Config::from_toml_str(&text, &pr.config_path()).unwrap();
            assert_eq!(parsed, Config::default_for(&pr), "profile {name:?}");
        }
    }

    #[test]
    fn round_trip_with_all_optional_fields() {
        let mut c = Config {
            name: "Zé \"o\" \\ Silva".into(),
            ..Config::default()
        };
        c.friends.push(Friend {
            name: "Ana".into(),
            address: "100.64.0.2".into(),
            port: Some(1234),
            fingerprint: Some("ab".repeat(32)),
        });
        c.soundboard.push(SoundConfig {
            file: "a.ogg".into(),
            hotkey: Some("Ctrl+Numpad1".into()),
            volume: 0.5,
        });
        c.audio.mode = AudioMode::PushToTalk;
        c.hotkeys.toggle_deafen = "Alt+F13".into();
        let text = c.to_commented_toml();
        assert_eq!(Config::from_toml_str(&text, Path::new("x")).unwrap(), c);
    }

    #[test]
    fn profiles_a_and_b_get_crossed_friend() {
        let root = temp_root("cross");
        let a = Config::load_or_create(&p(&root, Some("a"))).unwrap();
        let b = Config::load_or_create(&p(&root, Some("b"))).unwrap();
        assert_eq!(a.port, 47801);
        assert_eq!(b.port, 47802);
        assert_eq!(a.friends.len(), 1);
        assert_eq!(a.friends[0].name, "b");
        assert_eq!(a.friends[0].address, "127.0.0.1");
        assert_eq!(a.friends[0].effective_port(), 47802);
        assert_eq!(b.friends[0].name, "a");
        assert_eq!(b.friends[0].effective_port(), 47801);
        // negative control: default profile and other names have no friends
        assert!(Config::load_or_create(&p(&root, None)).unwrap().friends.is_empty());
        assert!(Config::load_or_create(&p(&root, Some("zz"))).unwrap().friends.is_empty());
    }

    #[test]
    fn partial_config_takes_defaults() {
        let c = Config::from_toml_str("name = \"X\"\n[audio]\nbitrate_kbps = 64\n", Path::new("x"))
            .unwrap();
        assert_eq!(c.name, "X");
        assert_eq!(c.audio.bitrate_kbps, 64);
        assert_eq!(c.audio.input_device, "default");
        assert_eq!(c.port, 47800);
        assert_eq!(c.hotkeys, HotkeyConfig::default());
        assert_eq!(c.hotkeys.toggle_deafen, "Ctrl+Shift+D");
        assert_eq!(c.game_mode, GameModeConfig::default());
        assert_eq!(
            Config::from_toml_str("", Path::new("x")).unwrap(),
            Config::default()
        );
        // friend without optional fields
        let c = Config::from_toml_str(
            "[[friends]]\nname = \"F\"\naddress = \"10.0.0.1\"\n",
            Path::new("x"),
        )
        .unwrap();
        assert_eq!(c.friends[0].port, None);
        assert_eq!(c.friends[0].effective_port(), 47800);
        assert_eq!(c.friends[0].fingerprint, None);
    }

    #[test]
    fn bitrate_is_clamped() {
        let load = |b: u32| {
            Config::from_toml_str(&format!("[audio]\nbitrate_kbps = {b}\n"), Path::new("x"))
                .unwrap()
                .audio
                .bitrate_kbps
        };
        assert_eq!(load(8), 32);
        assert_eq!(load(500), 128);
        assert_eq!(load(32), 32);
        assert_eq!(load(128), 128);
        assert_eq!(load(100), 100); // negative control: in range is untouched
    }

    #[test]
    fn invalid_address_is_clear_error() {
        let err = Config::from_toml_str(
            "[[friends]]\nname = \"Ana\"\naddress = \"100.x.y.z\"\n",
            Path::new("x"),
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::InvalidAddress { .. }));
        let msg = err.to_string();
        assert!(msg.contains("Ana") && msg.contains("100.x.y.z"), "{msg}");
        // negative control: valid IPv4/IPv6 pass
        for ok in ["127.0.0.1", "::1", "100.64.1.2"] {
            let t = format!("[[friends]]\nname = \"F\"\naddress = \"{ok}\"\n");
            assert!(Config::from_toml_str(&t, Path::new("x")).is_ok(), "{ok}");
        }
    }

    #[test]
    fn syntax_error_reports_path() {
        let err = Config::from_toml_str("name = ", Path::new("cfg.toml")).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
        assert!(err.to_string().contains("cfg.toml"));
    }

    #[test]
    fn second_load_does_not_overwrite_manual_edit() {
        let root = temp_root("noover");
        let pr = p(&root, Some("a"));
        Config::load_or_create(&pr).unwrap();
        fs::write(pr.config_path(), "name = \"Editado\"\nport = 5000\n").unwrap();
        let c = Config::load_or_create(&pr).unwrap();
        assert_eq!(c.name, "Editado");
        assert_eq!(c.port, 5000);
        assert!(c.friends.is_empty()); // not re-seeded
        assert_eq!(
            fs::read_to_string(pr.config_path()).unwrap(),
            "name = \"Editado\"\nport = 5000\n"
        );
    }

    #[test]
    fn save_then_load_round_trips() {
        let root = temp_root("save");
        let pr = p(&root, Some("a"));
        let mut c = Config::load_or_create(&pr).unwrap();
        c.name = "Novo".into();
        c.friends[0].fingerprint = Some("cd".repeat(32));
        c.save(&pr).unwrap();
        assert_eq!(Config::load_or_create(&pr).unwrap(), c);
    }
}
