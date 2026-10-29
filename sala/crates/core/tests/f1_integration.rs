//! F1 end-to-end: two `Session`s in one process (profiles a and b, 127.0.0.1, synthetic
//! audio). Voice delivery, chat both ways, `inv-chat-never-on-disk` (files and logs),
//! history dropped on `Disconnected`.

use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sala_core::config::{Config, Friend, Profile};
use sala_core::identity::LocalIdentity;
use sala_core::session::{AudioSource, Session};

const TEST_TIMEOUT: Duration = Duration::from_secs(60);
const CALL_TIME: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------- helpers

fn temp_root(tag: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "sala-f1-{tag}-{}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn free_udp_port() -> u16 {
    UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A unique, printable marker that cannot occur by chance.
fn marker(tag: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!(
        "MARK-{tag}-{:x}-{:x}-{:x}",
        nanos,
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// Collects every file under `dir` (recursively) containing `needle` as raw bytes.
fn files_containing(dir: &Path, needle: &[u8]) -> Vec<PathBuf> {
    let mut hits = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let bytes = std::fs::read(&path).unwrap();
                if bytes.windows(needle.len()).any(|w| w == needle) {
                    hits.push(path);
                }
            }
        }
    }
    hits
}

fn count_files(dir: &Path) -> usize {
    let mut n = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                n += 1;
            }
        }
    }
    n
}

#[derive(Clone, Default)]
struct LogBuf(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuf {
    type Writer = LogBuf;
    fn make_writer(&'a self) -> LogBuf {
        self.clone()
    }
}

async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !cond() {
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn with_timeout<T>(f: impl Future<Output = T>) -> T {
    tokio::time::timeout(TEST_TIMEOUT, f).await.expect("test timed out")
}

/// Creates profile `name` under `root` (config.toml + cert on disk), with the other
/// side as its only friend.
fn make_profile(root: &Path, name: &str, port: u16, other: &str, other_port: u16) -> (Profile, Config, LocalIdentity) {
    let profile = Profile::resolve_in(root, Some(name)).unwrap();
    let mut config = Config::load_or_create(&profile).unwrap();
    config.port = port;
    config.friends = vec![Friend {
        name: other.into(),
        address: "127.0.0.1".into(),
        port: Some(other_port),
        fingerprint: None,
    }];
    config.save(&profile).unwrap();
    let identity = LocalIdentity::load_or_create(&profile.data_dir).unwrap();
    (profile, config, identity)
}

fn start(profile: Profile, config: Config, identity: &LocalIdentity) -> Session {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, config.port));
    Session::start(profile, config, identity, addr, AudioSource::Synthetic).unwrap()
}

// ---------------------------------------------------------------- test

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn f1_call_voice_chat_and_chat_never_on_disk() {
    let logs = LogBuf::default();
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .finish(),
    )
    .expect("this test file runs a single test that owns the global subscriber");

    with_timeout(async {
        let root = temp_root("data");
        let (pa, pb) = (free_udp_port(), free_udp_port());
        let (profile_a, config_a, id_a) = make_profile(&root, "a", pa, "b", pb);
        let (profile_b, config_b, id_b) = make_profile(&root, "b", pb, "a", pa);
        let dir_a = profile_a.data_dir.clone();
        let dir_b = profile_b.data_dir.clone();
        let a = start(profile_a, config_a, &id_a);
        let b = start(profile_b, config_b, &id_b);

        wait_until("a and b connected", || a.is_connected("b") && b.is_connected("a")).await;

        // D-024: connected is not in the room; no voice until both join.
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(a.voice_counters("b").sent, 0, "control: nothing is sent before joining");
        a.set_in_room(true);
        b.set_in_room(true);

        // (i) ~5 s of call.
        tokio::time::sleep(CALL_TIME).await;

        // (ii) chat both ways with unique texts.
        let (m_ab, m_ba, m_ab2) = (marker("a2b"), marker("b2a"), marker("a2b-second"));
        let sent_ab = a.send_chat("b", &m_ab).unwrap();
        let sent_ba = b.send_chat("a", &m_ba).unwrap();
        a.send_chat("b", &m_ab2).unwrap();
        wait_until("chat delivered", || b.chat_history("a").len() == 3 && a.chat_history("b").len() == 3).await;

        let hist_a = a.chat_history("b");
        let hist_b = b.chat_history("a");
        let view = |h: &[sala_protocol::ChatEntry]| -> Vec<(bool, String)> {
            h.iter().map(|e| (e.from_self, e.text.clone())).collect()
        };
        let a_has = view(&hist_a);
        let b_has = view(&hist_b);
        assert!(a_has.contains(&(true, m_ab.clone())));
        assert!(a_has.contains(&(true, m_ab2.clone())));
        assert!(a_has.contains(&(false, m_ba.clone())));
        assert!(b_has.contains(&(false, m_ab.clone())));
        assert!(b_has.contains(&(false, m_ab2.clone())));
        assert!(b_has.contains(&(true, m_ba.clone())));
        assert_eq!(hist_a.iter().filter(|e| e.from_self).count(), 2);
        assert_eq!(hist_b.iter().filter(|e| e.from_self).count(), 1);
        // Same id on both ends; order of one sender is preserved.
        assert!(hist_b.iter().any(|e| e.id == sent_ab.id && !e.from_self));
        assert!(hist_a.iter().any(|e| e.id == sent_ba.id && !e.from_self));
        let pos = |h: &[sala_protocol::ChatEntry], t: &str| h.iter().position(|e| e.text == t).unwrap();
        assert!(pos(&hist_b, &m_ab) < pos(&hist_b, &m_ab2));
        // Unknown friend: error, nothing stored.
        assert!(a.send_chat("nobody", "x").is_err());

        // (i) voice delivery: read B's received first, then A's sent (in-flight frames
        // count against the ratio, never for it).
        let received = b.voice_counters("a").received;
        let sent = a.voice_counters("b").sent;
        let ratio = received as f64 / sent as f64;
        eprintln!("voice a->b: sent={sent} received={received} ratio={ratio:.4}");
        assert!(sent >= 200, "5 s of 20 ms frames expected, sent={sent}");
        assert!(ratio >= 0.95, "B received only {:.1}% of A's frames", ratio * 100.0);
        let reverse = a.voice_counters("b").received as f64 / b.voice_counters("a").sent as f64;
        assert!(reverse >= 0.95, "reverse direction delivered only {:.1}%", reverse * 100.0);

        // (iv) B leaves: A sees Disconnected and drops the history; B drops its own.
        assert!(!a.chat_history("b").is_empty(), "control: history exists during the call");
        b.shutdown().await;
        assert!(b.chat_history("a").is_empty(), "history dropped on shutdown");
        wait_until("a sees the disconnect", || !a.is_connected("b")).await;
        wait_until("a drops history after Disconnected", || a.chat_history("b").is_empty()).await;
        // Voice counters hold no content and survive the call.
        assert!(a.voice_counters("b").received > 0);

        a.shutdown().await;
        assert!(a.chat_history("b").is_empty());

        // (iii) inv-chat-never-on-disk: scan both profiles' data dirs (config, cert, all).
        let markers = [&m_ab, &m_ba, &m_ab2];
        assert!(count_files(&dir_a) >= 2 && count_files(&dir_b) >= 2, "profiles must have files to scan");
        for m in markers {
            for dir in [&dir_a, &dir_b, &root] {
                let hits = files_containing(dir, m.as_bytes());
                assert!(hits.is_empty(), "chat text found on disk: {hits:?}");
            }
        }
        // Control: the scanner does find a marker written to a file.
        let control_dir = temp_root("control");
        std::fs::write(control_dir.join("leak.txt"), format!("prefix {m_ab} suffix")).unwrap();
        assert_eq!(files_containing(&control_dir, m_ab.as_bytes()).len(), 1);
        let _ = std::fs::remove_dir_all(&control_dir);

        // Logs never contain chat text, yet the chat activity was logged (id/len only).
        let log_text = String::from_utf8_lossy(&logs.0.lock().unwrap()).into_owned();
        assert!(log_text.contains("event: Chat") && log_text.contains("chat sent"), "chat events must be logged");
        for m in markers {
            assert!(!log_text.contains(m.as_str()), "chat text found in logs");
        }
        // Control: a marker that IS logged shows up in the captured buffer.
        let logged = marker("logged-on-purpose");
        tracing::info!("control marker {logged}");
        let log_text = String::from_utf8_lossy(&logs.0.lock().unwrap()).into_owned();
        assert!(log_text.contains(logged.as_str()), "log capture must see what is logged");

        let _ = std::fs::remove_dir_all(&root);
    })
    .await;
}
