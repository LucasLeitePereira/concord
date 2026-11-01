//! Self-update (D-038), pure part: manifest signature, version and URL checks, installer hash.
//! No network and no disk beyond reading the file to hash; the download and the launch of the
//! installer come in T-041b/c. Order of trust: signature first, only then the JSON is read.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use minisign_verify::{PublicKey, Signature};
use sala_protocol::UpdateState;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{info, warn};

/// First check after the core starts.
pub const FIRST_CHECK_DELAY: Duration = Duration::from_secs(30);
/// Time between checks.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// Retry delay when a check was skipped because the game mode was on.
pub const GAME_MODE_RETRY: Duration = Duration::from_secs(10 * 60);

/// Whether the core talks to the network about updates: never for test profiles (`--profile`).
pub fn update_enabled(profile_name: Option<&str>) -> bool {
    profile_name.is_none()
}

/// What the background work tells the `Controller` (D-038).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateEvent {
    /// A check found a verified, newer release.
    Found(Manifest),
    /// Progress of a running install (`Downloading`, `Installing` or `Failed`).
    State(UpdateState),
    /// The user refused the UAC prompt: back to `Available`, nothing else changes.
    Cancelled,
}

/// Manifest published with every release (small JSON, signed with minisign).
pub const MANIFEST_URL: &str = "https://github.com/LucasLeitePereira/concord/releases/latest/download/latest.json";
/// Detached minisign signature of `latest.json`.
pub const SIGNATURE_URL: &str = "https://github.com/LucasLeitePereira/concord/releases/latest/download/latest.json.minisig";
/// Only installers under the releases of this repository are accepted.
pub const RELEASE_PREFIX: &str = "https://github.com/LucasLeitePereira/concord/releases/download/";
/// Larger manifests are refused before parsing.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
/// Larger installers are refused while downloading.
pub const MAX_INSTALLER_BYTES: u64 = 200 * 1024 * 1024;

/// Public key (the base64 line of the `.pub` file from `minisign -G`).
// Must match `sala/concord-update.pub`. Changing it means every friend reinstalls by hand once.
pub const PUBLIC_KEY: &str = "RWTbHhsEGnIz+Cf8qub5r8+/+W8Bo5mshILLA9lc+YFGvNKUuoaaAnGn";

/// The T-041d placeholder, whose private key was thrown away.
const PLACEHOLDER_PUBLIC_KEY: &str = "RWR0O4aPmu5Poj/rvw/TwLbSAxRH1NXV+KdmqMBdsARVU8GKuAeOldYo";

/// False if `PUBLIC_KEY` were the old placeholder: a release must not go out then.
pub fn release_key_ready() -> bool {
    PUBLIC_KEY != PLACEHOLDER_PUBLIC_KEY
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum UpdateError {
    #[error("manifest too large")]
    ManifestTooLarge,
    #[error("invalid public key")]
    BadPublicKey,
    #[error("invalid signature file")]
    BadSignatureFile,
    #[error("manifest signature does not verify")]
    BadSignature,
    #[error("manifest is not valid JSON: {0}")]
    BadManifest(String),
    #[error("installer URL not allowed: {0}")]
    UrlNotAllowed(String),
    #[error("version {candidate} is not newer than {current}")]
    NotNewer { candidate: String, current: String },
    #[error("sha256 must be 64 hex characters")]
    BadSha256,
    #[error("network: {0}")]
    Network(String),
    #[error("disk: {0}")]
    Io(String),
    #[error("download larger than the limit")]
    TooLarge,
    #[error("installer sha256 does not match the manifest")]
    HashMismatch,
    #[error("could not start the installer (Win32 error {0})")]
    Launch(u32),
}

/// `latest.json`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct Manifest {
    pub version: String,
    pub url: String,
    pub sha256: String,
    #[serde(default)]
    pub notes: String,
}

/// Checks the minisign signature of `manifest_bytes` BEFORE interpreting the JSON.
pub fn parse_and_verify_manifest(manifest_bytes: &[u8], signature_text: &str, public_key: &str) -> Result<Manifest, UpdateError> {
    if manifest_bytes.len() > MAX_MANIFEST_BYTES {
        return Err(UpdateError::ManifestTooLarge);
    }
    let key = PublicKey::from_base64(public_key.trim()).map_err(|_| UpdateError::BadPublicKey)?;
    let sig = Signature::decode(signature_text).map_err(|_| UpdateError::BadSignatureFile)?;
    // `false`: only prehashed signatures (what the minisign CLI produces); no legacy ones.
    key.verify(manifest_bytes, &sig, false).map_err(|_| UpdateError::BadSignature)?;
    serde_json::from_slice(manifest_bytes).map_err(|e| UpdateError::BadManifest(e.to_string()))
}

/// True when `candidate` is a valid semver strictly greater than `current` (invalid = false).
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (semver::Version::parse(candidate.trim()), semver::Version::parse(current.trim())) {
        (Ok(c), Ok(cur)) => c > cur,
        _ => false,
    }
}

/// True for URLs under `RELEASE_PREFIX` without tricks (path climbing, userinfo, query,
/// fragment, backslash, control characters, spaces, percent-escapes).
pub fn url_allowed(url: &str) -> bool {
    let Some(rest) = url.strip_prefix(RELEASE_PREFIX) else {
        return false;
    };
    !rest.is_empty()
        && !url.contains("..")
        && !rest.contains(['@', '?', '#', '\\', '%', ' '])
        && !url.chars().any(char::is_control)
        && url.is_ascii()
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Whole check of a verified manifest against the running version.
pub fn validate(manifest: &Manifest, current: &str) -> Result<(), UpdateError> {
    if !url_allowed(&manifest.url) {
        return Err(UpdateError::UrlNotAllowed(manifest.url.clone()));
    }
    if !is_newer(&manifest.version, current) {
        return Err(UpdateError::NotNewer { candidate: manifest.version.clone(), current: current.to_string() });
    }
    if !is_sha256_hex(&manifest.sha256) {
        return Err(UpdateError::BadSha256);
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// True when the SHA-256 of `bytes` equals `expected` (hex, case-insensitive).
pub fn verify_sha256(bytes: &[u8], expected: &str) -> bool {
    hex(&Sha256::digest(bytes)).eq_ignore_ascii_case(expected.trim())
}

/// Same as `verify_sha256`, streaming a reader to the end; read error = false.
pub fn verify_sha256_reader(mut reader: impl Read, expected: &str) -> bool {
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(_) => return false,
        }
    }
    hex(&hasher.finalize()).eq_ignore_ascii_case(expected.trim())
}

/// Same as `verify_sha256`, streaming a file; unreadable file = false.
pub fn verify_sha256_file(path: &Path, expected: &str) -> bool {
    match std::fs::File::open(path) {
        Ok(file) => verify_sha256_reader(file, expected),
        Err(_) => false,
    }
}

/// Turns a growing byte count into whole percents, reporting only when the value changes.
#[derive(Debug, Default)]
pub struct PercentTracker {
    last: Option<u8>,
}

impl PercentTracker {
    /// `Some(percent)` when it differs from the last one reported; `None` if unknown total or unchanged.
    pub fn update(&mut self, done: u64, total: Option<u64>) -> Option<u8> {
        let total = total.filter(|t| *t > 0)?;
        let pct = (u128::from(done) * 100 / u128::from(total)).min(100) as u8;
        if self.last == Some(pct) {
            return None;
        }
        self.last = Some(pct);
        Some(pct)
    }
}

/// Copies `reader` into `writer`, failing with `TooLarge` as soon as more than `limit` bytes
/// arrive (exactly `limit` is fine). `on_percent` gets each new whole percent of `total`.
pub fn copy_limited<R: Read, W: Write>(
    mut reader: R,
    mut writer: W,
    limit: u64,
    total: Option<u64>,
    mut on_percent: impl FnMut(u8),
) -> Result<u64, UpdateError> {
    if total.is_some_and(|t| t > limit) {
        return Err(UpdateError::TooLarge);
    }
    let mut tracker = PercentTracker::default();
    let mut buf = [0u8; 64 * 1024];
    let mut done = 0u64;
    loop {
        let n = reader.read(&mut buf).map_err(|e| UpdateError::Network(e.to_string()))?;
        if n == 0 {
            return Ok(done);
        }
        done += n as u64;
        if done > limit {
            return Err(UpdateError::TooLarge);
        }
        writer.write_all(&buf[..n]).map_err(|e| UpdateError::Io(e.to_string()))?;
        if let Some(p) = tracker.update(done, total) {
            on_percent(p);
        }
    }
}

/// HTTP agent: HTTPS only (redirects included: every hop is checked), few redirects, timeouts.
/// `global`: limit for the whole call (`None` for the installer, which has a stall timeout instead).
fn agent(global: Option<Duration>) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(5)
        .timeout_resolve(Some(Duration::from_secs(10)))
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_per_read(Some(Duration::from_secs(30)))
        .timeout_global(global)
        .user_agent(concat!("concord-updater/", env!("CARGO_PKG_VERSION")))
        .build();
    ureq::Agent::new_with_config(config)
}

fn get_limited(agent: &ureq::Agent, url: &str, limit: u64) -> Result<Vec<u8>, UpdateError> {
    let response = agent.get(url).call().map_err(|e| UpdateError::Network(e.to_string()))?;
    let total = response.body().content_length();
    let mut out = Vec::new();
    copy_limited(response.into_body().into_reader(), &mut out, limit, total, |_| {})?;
    Ok(out)
}

/// Downloads `latest.json` and its signature, verifies both and checks the release is newer
/// than the running version. Blocking: run it in `spawn_blocking`.
pub fn fetch_manifest() -> Result<Manifest, UpdateError> {
    let agent = agent(Some(Duration::from_secs(30)));
    let manifest = get_limited(&agent, MANIFEST_URL, MAX_MANIFEST_BYTES as u64)?;
    let signature = get_limited(&agent, SIGNATURE_URL, 4 * 1024)?;
    let signature = String::from_utf8(signature).map_err(|_| UpdateError::BadSignatureFile)?;
    let manifest = parse_and_verify_manifest(&manifest, &signature, PUBLIC_KEY)?;
    validate(&manifest, env!("CARGO_PKG_VERSION"))?;
    Ok(manifest)
}

/// `%LOCALAPPDATA%\Concord\update`.
pub fn update_dir() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty())?;
    Some(PathBuf::from(base).join("Concord").join("update"))
}

/// Deletes what an earlier update left in the update folder (errors ignored: the installer may
/// still be running from there).
pub fn cleanup_update_dir() {
    let Some(dir) = update_dir() else { return };
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    for entry in entries.flatten() {
        let _ = std::fs::remove_file(entry.path());
    }
}

/// Downloads the installer to `<update dir>\Concord-Setup-<version>.exe` (through a `.part` file).
fn download_installer(manifest: &Manifest, on_percent: impl FnMut(u8)) -> Result<PathBuf, UpdateError> {
    let dir = update_dir().ok_or_else(|| UpdateError::Io("LOCALAPPDATA is not set".into()))?;
    std::fs::create_dir_all(&dir).map_err(|e| UpdateError::Io(e.to_string()))?;
    let final_path = dir.join(format!("Concord-Setup-{}.exe", manifest.version));
    let part_path = dir.join(format!("Concord-Setup-{}.exe.part", manifest.version));
    let result = (|| {
        let response = agent(None).get(&manifest.url).call().map_err(|e| UpdateError::Network(e.to_string()))?;
        let total = response.body().content_length();
        let mut file = std::fs::File::create(&part_path).map_err(|e| UpdateError::Io(e.to_string()))?;
        copy_limited(response.into_body().into_reader(), &mut file, MAX_INSTALLER_BYTES, total, on_percent)?;
        file.sync_all().map_err(|e| UpdateError::Io(e.to_string()))?;
        drop(file);
        let _ = std::fs::remove_file(&final_path);
        std::fs::rename(&part_path, &final_path).map_err(|e| UpdateError::Io(e.to_string()))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&part_path);
    }
    result.map(|()| final_path)
}

/// Opens `path` read-only denying writers (`FILE_SHARE_READ`): while the handle lives, nobody can
/// change the file between the hash check and the launch.
fn open_locked(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 1;
    std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(path)
}

/// `ShellExecuteExW` with `verb`, then waits for the process to end and returns its exit code.
/// `Err(code)` is the Win32 error of the launch itself (1223 = declined, if Windows says so).
/// With the `open` verb the installer asks for UAC by itself (D-038): if the user declines, the
/// process just ends; if they accept, the installer kills this core before the wait returns.
fn shell_execute(verb: &str, file: &Path, params: &str) -> Result<u32, u32> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
    use windows_sys::Win32::UI::Shell::{
        SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    const ERROR_INVALID_HANDLE: u32 = 6;
    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }
    let verb = wide(verb.as_ref());
    let file = wide(file.as_os_str());
    let params = wide(params.as_ref());
    // SAFETY: zeroed is a valid SHELLEXECUTEINFOW; the buffers outlive the call.
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = SW_SHOWNORMAL;
    // SAFETY: `info` is fully initialised above.
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        // SAFETY: plain thread-local error read right after the failing call.
        return Err(unsafe { GetLastError() });
    }
    if info.hProcess.is_null() {
        return Err(ERROR_INVALID_HANDLE);
    }
    let mut code = 0u32;
    // SAFETY: `hProcess` is a valid process handle we own (NOCLOSEPROCESS); closed once below.
    unsafe {
        WaitForSingleObject(info.hProcess, INFINITE);
        GetExitCodeProcess(info.hProcess, &mut code);
        CloseHandle(info.hProcess);
    }
    Ok(code)
}

/// `ERROR_CANCELLED`: Windows reported a declined elevation prompt.
const ERROR_CANCELLED: u32 = 1223;

/// Result of handing the installer to Windows. Reaching either means the core is still alive,
/// so the installation did not happen (a successful one kills the core first).
#[derive(Debug, PartialEq, Eq)]
enum Launch {
    /// The installer process ended (for instance UAC declined) with this exit code.
    Exited(u32),
    /// `ShellExecuteExW` reported `ERROR_CANCELLED`.
    Declined,
}

/// Verifies the hash through a locked handle and launches `path` with `verb` while the handle
/// is still open.
fn verify_and_launch(path: &Path, expected_sha256: &str, verb: &str, params: &str) -> Result<Launch, UpdateError> {
    let file = open_locked(path).map_err(|e| UpdateError::Io(e.to_string()))?;
    if !verify_sha256_reader(&file, expected_sha256) {
        return Err(UpdateError::HashMismatch);
    }
    let outcome = match shell_execute(verb, path, params) {
        Ok(code) => Ok(Launch::Exited(code)),
        Err(ERROR_CANCELLED) => Ok(Launch::Declined),
        Err(code) => Err(UpdateError::Launch(code)),
    };
    drop(file);
    outcome
}

const MSG_DOWNLOAD: &str = "Não foi possível baixar a atualização. Verifique a conexão e tente de novo.";
const MSG_VERIFY: &str = "O arquivo baixado não passou na verificação e não foi instalado.";
const MSG_LAUNCH: &str = "Não foi possível iniciar a instalação.";

/// Download, check and launch the installer (blocking; run in `spawn_blocking`). Every outcome
/// goes to `send`; technical detail only to the log.
pub fn install_blocking(manifest: &Manifest, send: &dyn Fn(UpdateEvent)) {
    let failed = |message: &str| {
        send(UpdateEvent::State(UpdateState::Failed {
            version: manifest.version.clone(),
            notes: manifest.notes.clone(),
            message: message.to_string(),
        }));
    };
    let path = match download_installer(manifest, |percent| send(UpdateEvent::State(UpdateState::Downloading { percent }))) {
        Ok(p) => p,
        Err(e) => {
            warn!("update download failed: {e}");
            failed(MSG_DOWNLOAD);
            return;
        }
    };
    // Installing is announced before the UAC prompt; declining takes it back.
    send(UpdateEvent::State(UpdateState::Installing));
    match verify_and_launch(&path, &manifest.sha256, "open", "/SILENT /SUPPRESSMSGBOXES /NORESTART") {
        Ok(Launch::Exited(code)) => {
            // Still alive after the installer ended: nothing was installed (UAC declined, or it
            // failed early). There is no documented exit code for a declined UAC, so log it.
            info!("update installer ended with exit code {code} and the core is still running: not installed");
            send(UpdateEvent::Cancelled);
        }
        Ok(Launch::Declined) => {
            info!("update declined at the UAC prompt");
            send(UpdateEvent::Cancelled);
        }
        Err(e) => {
            warn!("update install failed: {e}");
            if e == UpdateError::HashMismatch {
                let _ = std::fs::remove_file(&path);
                failed(MSG_VERIFY);
            } else {
                failed(MSG_LAUNCH);
            }
        }
    }
}

/// How the `Controller` starts the install worker (tests replace it).
pub type UpdateInstaller = std::sync::Arc<dyn Fn(Manifest, UnboundedSender<UpdateEvent>) + Send + Sync>;

/// The real worker: `install_blocking` on a blocking thread.
pub fn real_installer() -> UpdateInstaller {
    std::sync::Arc::new(|manifest, tx| {
        tokio::task::spawn_blocking(move || install_blocking(&manifest, &|ev| drop(tx.send(ev))));
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fixture generated once with the `minisign` crate (not a dependency); the test private key
    // was discarded. `TEST_KEY` lives only here.
    const TEST_KEY: &str = "RWTrQZCzJXbz3H5t0lNEKGKoSO7q8d4LodGrsI8Bmt8HLAPgn7pss4QS";
    const MANIFEST: &str = r#"{"version":"0.1.12","url":"https://github.com/LucasLeitePereira/concord/releases/download/v0.1.12/Concord-0.1.12-setup.exe","sha256":"2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824","notes":"Correcoes"}"#;
    const SIGNATURE: &str = "untrusted comment: test key\nRUTrQZCzJXbz3MLMZyMjXyjM72rshZrr9NSc/QxyjOKY3qtp31W0Mm+N+JKlKngxg48lWKrvYsz3AOm536rtUbP8R6gPp+jiIgk=\ntrusted comment: test\nsEPQ72zQD9kyuyuH+mMo7MjpWSTp1OGgayxMROZnfM0PhMwpNVeyup4s/fOSKiAf3o/1FTA55GpbG7t6NiYwDQ==\n";
    /// SHA-256 of "hello".
    const HELLO_SHA: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    fn good() -> Manifest {
        parse_and_verify_manifest(MANIFEST.as_bytes(), SIGNATURE, TEST_KEY).expect("fixture must verify")
    }

    #[test]
    fn valid_signature_parses() {
        let m = good();
        assert_eq!(m.version, "0.1.12");
        assert_eq!(m.notes, "Correcoes");
        assert_eq!(validate(&m, "0.1.11"), Ok(()));
    }

    #[test]
    fn tampered_manifest_is_rejected() {
        let tampered = MANIFEST.replace("0.1.12", "9.9.9");
        assert_eq!(parse_and_verify_manifest(tampered.as_bytes(), SIGNATURE, TEST_KEY), Err(UpdateError::BadSignature));
    }

    #[test]
    fn signature_from_other_key_is_rejected() {
        // PUBLIC_KEY (placeholder) is not the key that signed the fixture.
        assert_eq!(parse_and_verify_manifest(MANIFEST.as_bytes(), SIGNATURE, PUBLIC_KEY), Err(UpdateError::BadSignature));
    }

    #[test]
    fn garbage_signature_and_key_are_rejected() {
        assert_eq!(parse_and_verify_manifest(MANIFEST.as_bytes(), "nope", TEST_KEY), Err(UpdateError::BadSignatureFile));
        assert_eq!(parse_and_verify_manifest(MANIFEST.as_bytes(), SIGNATURE, "nope"), Err(UpdateError::BadPublicKey));
    }

    #[test]
    fn oversized_manifest_is_rejected_before_anything() {
        let big = vec![b' '; MAX_MANIFEST_BYTES + 1];
        assert_eq!(parse_and_verify_manifest(&big, SIGNATURE, TEST_KEY), Err(UpdateError::ManifestTooLarge));
    }

    #[test]
    fn signature_is_checked_before_json() {
        // Signature is checked first: with a wrong signature the error is BadSignature even for junk.
        assert_eq!(parse_and_verify_manifest(b"not json", SIGNATURE, TEST_KEY), Err(UpdateError::BadSignature));
    }

    #[test]
    fn newer_versions_compare_numerically() {
        assert!(is_newer("0.1.12", "0.1.11"));
        assert!(is_newer("0.1.11", "0.1.9"), "text comparison would say 0.1.11 < 0.1.9");
        assert!(!is_newer("0.1.9", "0.1.11"));
        assert!(!is_newer("0.1.11", "0.1.11"));
        assert!(!is_newer("0.1.10", "0.1.11"));
        assert!(!is_newer("abc", "0.1.11"));
        assert!(!is_newer("0.1.12", "abc"));
        assert!(!is_newer("", "0.1.11"));
    }

    #[test]
    fn urls_must_be_in_this_repository_without_tricks() {
        let ok = format!("{RELEASE_PREFIX}v0.1.12/Concord-0.1.12-setup.exe");
        assert!(url_allowed(&ok));
        for bad in [
            "https://github.com/OutroUsuario/concord/releases/download/v1/x.exe",
            "http://github.com/LucasLeitePereira/concord/releases/download/v1/x.exe",
            "https://evil.com/LucasLeitePereira/concord/releases/download/v1/x.exe",
            "https://github.com/LucasLeitePereira/concord/releases/download/../../../evil/x.exe",
            "https://github.com/LucasLeitePereira/concord/releases/download/v1/x.exe?a=b",
            "https://github.com/LucasLeitePereira/concord/releases/download/v1/x.exe#f",
            "https://github.com/LucasLeitePereira/concord/releases/download/@evil.com/x.exe",
            "https://github.com/LucasLeitePereira/concord/releases/download/v1\\x.exe",
            "https://github.com/LucasLeitePereira/concord/releases/download/v1/%2e%2e/x.exe",
            "https://github.com/LucasLeitePereira/concord/releases/download/",
            "https://github.com/LucasLeitePereira/concord/releases/download/v1/x.exe\n",
            "",
        ] {
            assert!(!url_allowed(bad), "must be rejected: {bad:?}");
        }
    }

    #[test]
    fn validate_checks_version_url_and_hash() {
        let m = good();
        assert_eq!(validate(&m, "0.1.12"), Err(UpdateError::NotNewer { candidate: "0.1.12".into(), current: "0.1.12".into() }));
        assert!(matches!(validate(&m, "0.2.0"), Err(UpdateError::NotNewer { .. })));
        let m_url = Manifest { url: "https://evil.com/x.exe".into(), ..m.clone() };
        assert!(matches!(validate(&m_url, "0.1.11"), Err(UpdateError::UrlNotAllowed(_))));
        for bad in ["abc", &HELLO_SHA[..63], &format!("{}g", &HELLO_SHA[..63]), ""] {
            let m_sha = Manifest { sha256: bad.to_string(), ..m.clone() };
            assert_eq!(validate(&m_sha, "0.1.11"), Err(UpdateError::BadSha256), "{bad:?}");
        }
    }

    #[test]
    fn sha256_matches_ignoring_case() {
        assert!(verify_sha256(b"hello", HELLO_SHA));
        assert!(verify_sha256(b"hello", &HELLO_SHA.to_uppercase()));
        assert!(!verify_sha256(b"hellp", HELLO_SHA));
        assert!(!verify_sha256(b"hello", &"0".repeat(64)));
        assert!(!verify_sha256(b"hello", ""));
    }

    #[test]
    fn sha256_of_file() {
        let path = std::env::temp_dir().join(format!("concord-update-test-{}.bin", std::process::id()));
        std::fs::write(&path, b"hello").unwrap();
        assert!(verify_sha256_file(&path, HELLO_SHA));
        assert!(!verify_sha256_file(&path, &"0".repeat(64)));
        std::fs::remove_file(&path).unwrap();
        assert!(!verify_sha256_file(&path, HELLO_SHA), "missing file must not verify");
    }

    #[test]
    fn copy_limited_accepts_exactly_the_limit_and_refuses_one_more() {
        let data = vec![7u8; 1000];
        let mut out = Vec::new();
        assert_eq!(copy_limited(&data[..], &mut out, 1000, None, |_| {}), Ok(1000));
        assert_eq!(out, data);
        let mut out = Vec::new();
        assert_eq!(copy_limited(&data[..], &mut out, 999, None, |_| {}), Err(UpdateError::TooLarge));
        // A declared length above the limit is refused before reading anything.
        let mut out = Vec::new();
        assert_eq!(copy_limited(&b"x"[..], &mut out, 10, Some(11), |_| {}), Err(UpdateError::TooLarge));
        assert!(out.is_empty());
    }

    #[test]
    fn percent_is_reported_only_when_it_changes() {
        let data = vec![1u8; 200_000];
        let mut seen = Vec::new();
        copy_limited(&data[..], std::io::sink(), 1 << 20, Some(200_000), |p| seen.push(p)).unwrap();
        assert!(seen.windows(2).all(|w| w[0] < w[1]), "repeated or decreasing: {seen:?}");
        assert_eq!(seen.last(), Some(&100));
        let mut t = PercentTracker::default();
        assert_eq!(t.update(1, Some(1000)), Some(0));
        assert_eq!(t.update(5, Some(1000)), None, "same percent");
        assert_eq!(t.update(10, Some(1000)), Some(1));
        assert_eq!(t.update(10, None), None, "unknown total");
        assert_eq!(t.update(5000, Some(1000)), Some(100), "capped at 100");
    }

    #[test]
    fn test_profiles_never_check_for_updates() {
        assert!(update_enabled(None));
        assert!(!update_enabled(Some("a")));
    }

    #[test]
    fn plain_http_is_refused_without_touching_the_network() {
        let err = agent(None).get("http://127.0.0.1:9/x").call().unwrap_err();
        assert!(matches!(err, ureq::Error::RequireHttpsOnly(_)), "{err:?}");
    }

    fn temp_file(name: &str, content: &[u8]) -> PathBuf {
        let path = std::env::temp_dir().join(format!("concord-update-{}-{name}", std::process::id()));
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn locked_handle_denies_writers_while_open() {
        let path = temp_file("locked.bin", b"hello");
        let handle = open_locked(&path).unwrap();
        assert!(std::fs::OpenOptions::new().write(true).open(&path).is_err(), "a writer got through the lock");
        assert!(std::fs::read(&path).is_ok(), "readers (the loader) must still work");
        drop(handle);
        assert!(std::fs::OpenOptions::new().write(true).open(&path).is_ok(), "control: free after the handle closes");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn wrong_hash_never_launches() {
        // Launching a missing verb/file would error with Launch(..); HashMismatch proves we stopped before.
        let path = temp_file("mismatch.bin", b"hello");
        assert_eq!(verify_and_launch(&path, &"0".repeat(64), "open", ""), Err(UpdateError::HashMismatch));
        std::fs::remove_file(&path).unwrap();
    }

    /// Manual: runs a harmless exe through `ShellExecuteExW` while the locked handle is open,
    /// proving Windows executes a file we hold with `FILE_SHARE_READ`.
    /// `cargo test -p sala-core --lib update::tests::manual -- --ignored`
    #[test]
    #[ignore = "launches a program (whoami.exe)"]
    fn manual_launch_with_locked_handle_open() {
        let src = Path::new(&std::env::var("SystemRoot").unwrap()).join("System32").join("whoami.exe");
        let path = std::env::temp_dir().join(format!("concord-update-{}-whoami.exe", std::process::id()));
        std::fs::copy(src, &path).unwrap();
        let sha = {
            let bytes = std::fs::read(&path).unwrap();
            hex(&Sha256::digest(&bytes))
        };
        let outcome = verify_and_launch(&path, &sha, "open", "/?");
        // whoami ends by itself: Exited with its code (0). A real Setup would have killed us first.
        assert_eq!(outcome, Ok(Launch::Exited(0)));
    }

    #[test]
    fn public_key_matches_the_pub_file() {
        let pub_file = include_str!("../../../concord-update.pub");
        assert_eq!(pub_file.lines().nth(1).map(str::trim), Some(PUBLIC_KEY));
    }

    /// Run by the release script: `cargo test -p sala-core --lib update -- --ignored`.
    #[test]
    #[ignore = "run by the release script"]
    fn release_requires_real_public_key() {
        assert!(release_key_ready(), "PUBLIC_KEY is still the placeholder");
    }
}
