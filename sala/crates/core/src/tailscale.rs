//! Brings Tailscale up when the core starts (T-020): opens the tray app if it is not
//! running and runs `tailscale up` when the backend is stopped. Never blocks start-up
//! (the caller runs [`ensure_up`] in the background) and never shows a console window.
//!
//! The decision (state -> step) is the pure [`next_step`]; the rest only executes it.

use std::io::Read;
use std::net::IpAddr;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tracing::{info, warn};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const DETACHED_PROCESS: u32 = 0x0000_0008;
const STATUS_TIMEOUT: Duration = Duration::from_secs(5);
const UP_TIMEOUT: Duration = Duration::from_secs(15);
/// How long to wait for `Running` after the actions (the user may be logging in).
const RUNNING_WAIT: Duration = Duration::from_secs(90);
const RUNNING_POLL: Duration = Duration::from_secs(2);

/// Tailscale's `BackendState`, reduced to what matters here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Running,
    Stopped,
    NeedsLogin,
    /// Anything else (`Starting`, `NoState`, ...) or no answer: wait, do nothing.
    Other,
}

/// What [`next_step`] asks for on one poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Backend is `Running`: stop polling.
    Done,
    /// Start `tailscale-ipn.exe` (the tray app).
    StartTrayApp,
    /// Run `tailscale up`.
    Up,
    /// Log that the user has to sign in on the tray app.
    NoteNeedsLogin,
    /// Nothing to do yet (`NoState`, `Starting`, retry pacing).
    Wait,
}

/// Minimum time between two tray app starts or two `tailscale up`.
const RETRY_GAP: Duration = Duration::from_secs(10);

/// Pure decision for one poll (T-029). Polled until `Running`, because right after the
/// tray app starts the backend is `NoState` and only later turns `Stopped`; a one-shot
/// decision missed that and never ran `up`. `since_*` = time since that action last ran.
pub fn next_step(backend: Backend, tray_running: bool, since_tray: Option<Duration>, since_up: Option<Duration>) -> Step {
    let due = |since: Option<Duration>| since.is_none_or(|d| d >= RETRY_GAP);
    match backend {
        Backend::Running => Step::Done,
        _ if !tray_running && due(since_tray) => Step::StartTrayApp,
        Backend::Stopped if due(since_up) => Step::Up,
        Backend::NeedsLogin => Step::NoteNeedsLogin,
        _ => Step::Wait,
    }
}

/// `BackendState` from the output of `tailscale status --json` (no JSON dependency:
/// the field is a plain string at the top level).
pub fn parse_backend(json: &str) -> Backend {
    let Some(i) = json.find("\"BackendState\"") else { return Backend::Other };
    let rest = json[i + "\"BackendState\"".len()..].trim_start();
    let Some(rest) = rest.strip_prefix(':') else { return Backend::Other };
    let rest = rest.trim_start();
    let Some(rest) = rest.strip_prefix('"') else { return Backend::Other };
    match rest.split('"').next().unwrap_or("") {
        "Running" => Backend::Running,
        "Stopped" => Backend::Stopped,
        "NeedsLogin" => Backend::NeedsLogin,
        _ => Backend::Other,
    }
}

fn install_dir() -> Option<PathBuf> {
    let base = std::env::var_os("ProgramFiles")?;
    Some(PathBuf::from(base).join("Tailscale"))
}

/// Runs a command without a window, with a timeout; returns stdout on success.
fn run_capture(cmd: Command, timeout: Duration) -> Option<String> {
    run_output(cmd, timeout).and_then(|(ok, out)| ok.then_some(out))
}

/// Like [`run_capture`], but also returns stdout of a failed exit (None only on timeout or
/// spawn failure): `tailscale status` exits non-zero while stopped yet still prints the state.
fn run_output(mut cmd: Command, timeout: Duration) -> Option<(bool, String)> {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(CREATE_NO_WINDOW).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = cmd.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    // Read on a thread so a full pipe cannot stall the wait loop.
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = reader.join().ok()?;
                return Some((status.success(), out));
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

fn tray_app_running() -> bool {
    let mut cmd = Command::new("tasklist");
    cmd.args(["/FI", "IMAGENAME eq tailscale-ipn.exe", "/NH", "/FO", "CSV"]);
    run_capture(cmd, STATUS_TIMEOUT).is_some_and(|o| o.to_ascii_lowercase().contains("tailscale-ipn.exe"))
}

fn backend(cli: &PathBuf) -> Backend {
    let mut cmd = Command::new(cli);
    cmd.args(["status", "--json"]);
    run_output(cmd, STATUS_TIMEOUT).map_or(Backend::Other, |(_, o)| parse_backend(&o))
}

fn start_tray_app(dir: &std::path::Path) {
    use std::os::windows::process::CommandExt;
    let exe = dir.join("tailscale-ipn.exe");
    match Command::new(&exe)
        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(_) => info!("tailscale: tray app started"),
        Err(e) => warn!("tailscale: could not start the tray app: {e}"),
    }
}

/// Blocking: polls the state and does what [`next_step`] says until `Running` (bounded).
/// Returns true when the backend ended up `Running`.
fn ensure_up_blocking() -> bool {
    let Some(dir) = install_dir().filter(|d| d.join("tailscale.exe").exists()) else {
        info!("tailscale: not installed; skipping");
        return false;
    };
    let cli = dir.join("tailscale.exe");
    let deadline = Instant::now() + RUNNING_WAIT;
    let (mut last_tray, mut last_up): (Option<Instant>, Option<Instant>) = (None, None);
    let mut noted_login = false;
    loop {
        let state = backend(&cli);
        // Only ask for the process list when it matters (it costs a tasklist run).
        let tray = state == Backend::Running || tray_app_running();
        let since = |t: Option<Instant>| t.map(|t| t.elapsed());
        match next_step(state, tray, since(last_tray), since(last_up)) {
            Step::Done => {
                info!("tailscale: running");
                return true;
            }
            Step::StartTrayApp => {
                start_tray_app(&dir);
                last_tray = Some(Instant::now());
            }
            Step::Up => {
                let mut cmd = Command::new(&cli);
                cmd.arg("up");
                match run_capture(cmd, UP_TIMEOUT) {
                    Some(_) => info!("tailscale: up done"),
                    None => warn!("tailscale: `tailscale up` failed or timed out"),
                }
                last_up = Some(Instant::now());
            }
            Step::NoteNeedsLogin if !noted_login => {
                info!("tailscale: needs login; sign in on the Tailscale tray app");
                noted_login = true;
            }
            Step::NoteNeedsLogin | Step::Wait => {}
        }
        if Instant::now() >= deadline {
            warn!(?state, "tailscale: not running after {RUNNING_WAIT:?}; giving up");
            return false;
        }
        std::thread::sleep(RUNNING_POLL);
    }
}

/// Background task: true when Tailscale is `Running` at the end.
pub async fn ensure_up() -> bool {
    tokio::task::spawn_blocking(ensure_up_blocking).await.unwrap_or(false)
}

/// True for Tailscale's address range (100.64.0.0/10, IPv4) and fd7a:115c:a1e0::/48 (IPv6).
pub fn in_tailnet_range(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            a == 100 && (64..128).contains(&b)
        }
        IpAddr::V6(v6) => v6.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}

/// Whether `ip` is one of the peers in `tailscale status --json` (own tailnet or machines
/// shared with us). No JSON dependency: looks for the quoted address after the `"Peer"` key
/// (routes are written as `"a.b.c.d/32"`, so they never match).
pub fn peer_listed(json: &str, ip: IpAddr) -> bool {
    let Some(i) = json.find("\"Peer\"") else { return false };
    json[i..].contains(&format!("\"{ip}\""))
}

fn is_peer_blocking(ip: IpAddr) -> Option<bool> {
    let dir = install_dir().filter(|d| d.join("tailscale.exe").exists())?;
    let mut cmd = Command::new(dir.join("tailscale.exe"));
    cmd.args(["status", "--json"]);
    let (_, out) = run_output(cmd, STATUS_TIMEOUT)?;
    // Peers are only meaningful while running.
    (parse_backend(&out) == Backend::Running).then(|| peer_listed(&out, ip))
}

/// `Some(false)` when Tailscale runs and `ip` is not reachable through it (friend on another
/// account without sharing); `None` when that cannot be told.
pub async fn is_peer(ip: IpAddr) -> Option<bool> {
    tokio::task::spawn_blocking(move || is_peer_blocking(ip)).await.ok().flatten()
}

#[derive(serde::Deserialize)]
struct StatusJson {
    #[serde(rename = "Self")]
    me: Option<NodeJson>,
    #[serde(rename = "Peer", default)]
    peer: Option<std::collections::HashMap<String, NodeJson>>,
    #[serde(rename = "User", default)]
    user: Option<std::collections::HashMap<String, UserJson>>,
}

#[derive(serde::Deserialize)]
struct NodeJson {
    #[serde(rename = "HostName", default)]
    host_name: String,
    #[serde(rename = "OS", default)]
    os: String,
    #[serde(rename = "UserID", default)]
    user_id: i64,
    #[serde(rename = "TailscaleIPs", default)]
    ips: Option<Vec<String>>,
    #[serde(rename = "Online", default)]
    online: bool,
}

#[derive(serde::Deserialize)]
struct UserJson {
    #[serde(rename = "DisplayName", default)]
    display_name: String,
    #[serde(rename = "LoginName", default)]
    login_name: String,
}

/// Devices listed by `tailscale status --json` (not this PC), online first, then by name.
/// Devices without an IPv4 address are skipped (Concord dials IPv4).
pub fn parse_peers(json: &str) -> Vec<sala_protocol::TailnetPeer> {
    let Ok(status) = serde_json::from_str::<StatusJson>(json) else { return Vec::new() };
    let me = status.me.map(|n| n.user_id);
    let users = status.user.unwrap_or_default();
    let mut out: Vec<_> = status
        .peer
        .unwrap_or_default()
        .into_values()
        .filter_map(|n| {
            let address = n.ips.unwrap_or_default().into_iter().find(|ip| ip.parse::<std::net::Ipv4Addr>().is_ok())?;
            let owner = users.get(&n.user_id.to_string()).and_then(|u| {
                let name = if u.display_name.is_empty() { &u.login_name } else { &u.display_name };
                (!name.is_empty()).then(|| name.clone())
            });
            Some(sala_protocol::TailnetPeer {
                name: n.host_name,
                address,
                online: n.online,
                owner,
                mine: me == Some(n.user_id),
                os: n.os,
            })
        })
        .collect();
    out.sort_by(|a, b| b.online.cmp(&a.online).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    out
}

/// Blocking: `(running, devices)` of this PC's Tailscale network.
pub fn list_peers_blocking() -> (bool, Vec<sala_protocol::TailnetPeer>) {
    let Some(dir) = install_dir().filter(|d| d.join("tailscale.exe").exists()) else { return (false, Vec::new()) };
    let mut cmd = Command::new(dir.join("tailscale.exe"));
    cmd.args(["status", "--json"]);
    match run_output(cmd, STATUS_TIMEOUT) {
        Some((_, out)) if parse_backend(&out) == Backend::Running => (true, parse_peers(&out)),
        _ => (false, Vec::new()),
    }
}

/// Blocking: `tailscale down` and closes the tray app (the service stays installed).
fn shut_down_blocking() {
    let Some(dir) = install_dir().filter(|d| d.join("tailscale.exe").exists()) else { return };
    let mut cmd = Command::new(dir.join("tailscale.exe"));
    cmd.arg("down");
    match run_capture(cmd, UP_TIMEOUT) {
        Some(_) => info!("tailscale: down done"),
        None => warn!("tailscale: `tailscale down` failed or timed out"),
    }
    let mut cmd = Command::new("taskkill");
    cmd.args(["/F", "/IM", "tailscale-ipn.exe"]);
    if run_capture(cmd, STATUS_TIMEOUT).is_some() {
        info!("tailscale: tray app closed");
    }
}

/// On quit: Tailscale goes down together with the app (user's request, D-018).
pub async fn shut_down() {
    let _ = tokio::task::spawn_blocking(shut_down_blocking).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: fn(u64) -> Option<Duration> = |s| Some(Duration::from_secs(s));

    #[test]
    fn reopening_after_quit_reaches_up_once_the_backend_is_stopped() {
        use Step::*;
        // Real sequence after Concord quit (`down` + tray app killed): tray off, NoState...
        assert_eq!(next_step(Backend::Other, false, None, None), StartTrayApp);
        // ...tray starting, still NoState: wait (the old one-shot decision ended here).
        assert_eq!(next_step(Backend::Other, true, S(2), None), Wait);
        // ...then Stopped: run `up`.
        assert_eq!(next_step(Backend::Stopped, true, S(4), None), Up);
        assert_eq!(next_step(Backend::Running, true, S(6), S(2)), Done);
    }

    #[test]
    fn retries_are_paced() {
        use Step::*;
        // Control: `up` is not repeated right away, nor the tray app restarted.
        assert_eq!(next_step(Backend::Stopped, true, None, S(3)), Wait);
        assert_eq!(next_step(Backend::Stopped, true, None, S(10)), Up);
        assert_eq!(next_step(Backend::Other, false, S(3), None), Wait);
        assert_eq!(next_step(Backend::Other, false, S(12), None), StartTrayApp);
        assert_eq!(next_step(Backend::NeedsLogin, true, None, None), NoteNeedsLogin);
        assert_eq!(next_step(Backend::Running, false, None, None), Done);
    }

    #[test]
    fn friend_outside_the_tailnet_is_detected() {
        let json = r#"{"BackendState":"Running","Self":{"TailscaleIPs":["100.70.1.1"]},
            "Peer":{"nodekey:a":{"HostName":"pc","TailscaleIPs":["100.76.130.127","fd7a:115c:a1e0::1"],
            "PrimaryRoutes":["100.74.137.67/32"]}}}"#;
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        assert!(peer_listed(json, ip("100.76.130.127")));
        assert!(peer_listed(json, ip("fd7a:115c:a1e0::1")));
        // Control: another tailnet's address, a route and our own address are not peers.
        assert!(!peer_listed(json, ip("100.74.137.67")));
        assert!(!peer_listed(json, ip("100.70.1.1")));
        assert!(!peer_listed(r#"{"Peer":null}"#, ip("100.76.130.127")));
    }

    #[test]
    fn lists_tailnet_devices_with_owner() {
        let json = r#"{"BackendState":"Running","Self":{"HostName":"meu-pc","UserID":1,"TailscaleIPs":["100.70.1.1"]},
            "Peer":{
              "nodekey:a":{"HostName":"notebook","OS":"windows","UserID":1,"TailscaleIPs":["100.70.1.2"],"Online":false},
              "nodekey:b":{"HostName":"DESKTOP-LUCAO","OS":"windows","UserID":2,"TailscaleIPs":["fd7a:115c:a1e0::9","100.76.130.127"],"Online":true},
              "nodekey:c":{"HostName":"so-ipv6","UserID":2,"TailscaleIPs":["fd7a:115c:a1e0::8"],"Online":true}},
            "User":{"1":{"DisplayName":"Eu","LoginName":"eu@x"},"2":{"DisplayName":"","LoginName":"lucao@x"}}}"#;
        let peers = parse_peers(json);
        let names: Vec<_> = peers.iter().map(|p| p.name.as_str()).collect();
        // Online first; this PC and IPv6-only devices are not listed.
        assert_eq!(names, ["DESKTOP-LUCAO", "notebook"]);
        assert_eq!(peers[0].address, "100.76.130.127");
        assert_eq!(peers[0].owner.as_deref(), Some("lucao@x"));
        assert!(!peers[0].mine && peers[1].mine);
        assert_eq!(peers[1].owner.as_deref(), Some("Eu"));
        assert!(parse_peers(r#"{"BackendState":"Running","Peer":null}"#).is_empty());
        assert!(parse_peers("garbage").is_empty());
    }

    #[test]
    fn tailnet_range() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        assert!(in_tailnet_range(ip("100.64.0.1")));
        assert!(in_tailnet_range(ip("100.127.255.254")));
        assert!(!in_tailnet_range(ip("100.128.0.1")));
        assert!(!in_tailnet_range(ip("127.0.0.1")));
        assert!(!in_tailnet_range(ip("192.168.0.10")));
        assert!(in_tailnet_range(ip("fd7a:115c:a1e0::1")));
    }

    #[test]
    fn parses_backend_state() {
        assert_eq!(parse_backend("{\n  \"Version\": \"1\",\n  \"BackendState\": \"Stopped\",\n"), Backend::Stopped);
        assert_eq!(parse_backend("{\"BackendState\":\"Running\"}"), Backend::Running);
        assert_eq!(parse_backend("{\"BackendState\": \"NeedsLogin\"}"), Backend::NeedsLogin);
        assert_eq!(parse_backend("{\"BackendState\": \"Starting\"}"), Backend::Other);
        assert_eq!(parse_backend("not json"), Backend::Other);
        assert_eq!(parse_backend(""), Backend::Other);
    }
}
