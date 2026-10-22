//! Integration tests for the QUIC peer connection (T-007, D-007).
//! Two endpoints in the same process on 127.0.0.1 with ephemeral ports.

use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use sala_core::config::Friend;
use sala_core::identity::{ConnSide, Fingerprint, FriendEntry, LocalIdentity, TrustStore};
use sala_core::net::{Net, NetError, NetEvent};
use sala_protocol::{AudioStream, Capabilities, ControlMessage, MediaDatagram, ProtocolError};
use tokio::sync::mpsc::UnboundedReceiver;

const LOCALHOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const TEST_TIMEOUT: Duration = Duration::from_secs(10);

struct Node {
    net: Net,
    rx: UnboundedReceiver<NetEvent>,
    identity: LocalIdentity,
}

fn new_identity() -> LocalIdentity {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "sala-net-test-{}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let id = LocalIdentity::load_or_create(&dir).unwrap();
    let _ = std::fs::remove_dir_all(dir);
    id
}

fn entry(name: &str, address: IpAddr, fingerprint: Option<Fingerprint>) -> FriendEntry {
    FriendEntry {
        name: name.into(),
        address,
        fingerprint,
    }
}

fn node(identity: LocalIdentity, friends: Vec<FriendEntry>, port: u16) -> Node {
    let addr = SocketAddr::new(LOCALHOST, port);
    let (net, rx) = Net::bind(addr, &identity, TrustStore::new(friends)).unwrap();
    Node { net, rx, identity }
}

fn friend(name: &str, port: u16) -> Friend {
    Friend {
        name: name.into(),
        address: "127.0.0.1".into(),
        port: Some(port),
        fingerprint: None,
    }
}

fn port(n: &Node) -> u16 {
    n.net.local_addr().port()
}

async fn with_timeout<T>(f: impl Future<Output = T>) -> T {
    tokio::time::timeout(TEST_TIMEOUT, f)
        .await
        .expect("test timed out")
}

/// Receives events (discarding the ones that do not match) until `pick` returns Some.
async fn wait_for<T>(
    rx: &mut UnboundedReceiver<NetEvent>,
    mut pick: impl FnMut(NetEvent) -> Option<T>,
) -> T {
    loop {
        let ev = rx.recv().await.expect("event channel closed");
        if let Some(v) = pick(ev) {
            return v;
        }
    }
}

async fn wait_connected(n: &mut Node, who: &str) -> (Fingerprint, bool) {
    wait_for(&mut n.rx, |e| match e {
        NetEvent::Connected {
            friend,
            fingerprint,
            newly_pinned,
        } if friend == who => Some((fingerprint, newly_pinned)),
        _ => None,
    })
    .await
}

fn hello(name: &str) -> ControlMessage {
    ControlMessage::Hello {
        name: name.into(),
        app_version: "test".into(),
        protocol_version: 1,
        capabilities: Capabilities::default(),
    }
}

fn status() -> ControlMessage {
    ControlMessage::Status {
        muted: true,
        deafened: false,
        game_mode: false,
        sharing: false,
    }
}

fn audio(len: usize) -> MediaDatagram {
    MediaDatagram::Audio {
        stream: AudioStream::Voice,
        seq: 7,
        timestamp: 960,
        payload: vec![0xAB; len],
    }
}

async fn wait_control(n: &mut Node, from: &str) -> ControlMessage {
    wait_for(&mut n.rx, |e| match e {
        NetEvent::Control { friend, msg } if friend == from => Some(msg),
        _ => None,
    })
    .await
}

/// A pair that already trusts each other's address, nothing pinned.
fn pair() -> (Node, Node) {
    let a = node(new_identity(), vec![entry("B", LOCALHOST, None)], 0);
    let b = node(new_identity(), vec![entry("A", LOCALHOST, None)], 0);
    (a, b)
}

#[tokio::test]
async fn a_connect_tofu_exchange_and_datagram() {
    with_timeout(async {
        let (mut a, mut b) = pair();
        a.net.dial(&friend("B", port(&b))).unwrap();

        let (fp_seen_by_a, pinned_a) = wait_connected(&mut a, "B").await;
        let (fp_seen_by_b, pinned_b) = wait_connected(&mut b, "A").await;
        assert!(pinned_a && pinned_b, "first contact must pin (TOFU)");
        assert_eq!(fp_seen_by_a, b.identity.fingerprint());
        assert_eq!(fp_seen_by_b, a.identity.fingerprint());
        assert_eq!(a.net.pinned_fingerprint("B"), Some(b.identity.fingerprint()));
        assert_eq!(b.net.pinned_fingerprint("A"), Some(a.identity.fingerprint()));
        assert_eq!(a.net.connection_side("B"), Some(ConnSide::Dialed));
        assert_eq!(b.net.connection_side("A"), Some(ConnSide::Accepted));

        a.net.send_control("B", hello("alice")).unwrap();
        a.net.send_control("B", status()).unwrap();
        b.net.send_control("A", hello("bob")).unwrap();
        assert_eq!(wait_control(&mut b, "A").await, hello("alice"));
        assert_eq!(wait_control(&mut b, "A").await, status());
        assert_eq!(wait_control(&mut a, "B").await, hello("bob"));

        // Datagrams flow both ways.
        let dg = audio(100);
        b.net.send_datagram("A", &dg).unwrap();
        let got = wait_for(&mut a.rx, |e| match e {
            NetEvent::Datagram { friend, dg } if friend == "B" => Some(dg),
            _ => None,
        })
        .await;
        assert_eq!(got, dg);
        a.net.send_datagram("B", &dg).unwrap();
        let got = wait_for(&mut b.rx, |e| match e {
            NetEvent::Datagram { dg, .. } => Some(dg),
            _ => None,
        })
        .await;
        assert_eq!(got, dg);

        // Second connection after a clean shutdown is no longer "newly pinned".
        a.net.shutdown().await;
        b.net.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn b_non_friend_is_rejected() {
    with_timeout(async {
        let mut a = node(new_identity(), vec![entry("B", LOCALHOST, None)], 0);
        // B only knows a different address, not 127.0.0.1.
        let other: IpAddr = "100.64.0.9".parse().unwrap();
        let mut b = node(new_identity(), vec![entry("Other", other, None)], 0);
        a.net.dial(&friend("B", port(&b))).unwrap();

        let rejected = wait_for(&mut b.rx, |e| match e {
            NetEvent::Rejected { addr, .. } => Some(addr),
            _ => None,
        })
        .await;
        assert_eq!(rejected.ip(), LOCALHOST);

        // A never connects; it keeps retrying with backoff.
        let attempt = wait_for(&mut a.rx, |e| match e {
            NetEvent::Connected { .. } => panic!("A must not connect to a peer that rejects it"),
            NetEvent::Reconnecting { attempt, delay, .. } => Some((attempt, delay)),
            _ => None,
        })
        .await;
        assert_eq!(attempt, (1, Duration::from_secs(1)));
        assert!(!a.net.is_connected("B"));
        assert_eq!(b.net.pinned_fingerprint("Other"), None);
    })
    .await;
}

#[tokio::test]
async fn c_post_handshake_fingerprint_mismatch_keeps_pin() {
    with_timeout(async {
        let wrong = Fingerprint([0x42; 32]);
        let a = node(new_identity(), vec![entry("B", LOCALHOST, None)], 0);
        let mut b = node(new_identity(), vec![entry("A", LOCALHOST, Some(wrong))], 0);
        a.net.dial(&friend("B", port(&b))).unwrap();

        let (name, expected, presented) = wait_for(&mut b.rx, |e| match e {
            NetEvent::Connected { .. } => panic!("B must refuse the mismatching peer"),
            NetEvent::FingerprintMismatch {
                friend,
                expected,
                presented,
            } => Some((friend, expected, presented)),
            _ => None,
        })
        .await;
        assert_eq!(name, "A");
        assert_eq!(expected, wrong);
        assert_eq!(presented, a.identity.fingerprint());
        assert_eq!(b.net.pinned_fingerprint("A"), Some(wrong), "pin must not change");
        assert!(!b.net.is_connected("A"));
        // Control: with the right pin the same setup connects (see test a).
    })
    .await;
}

/// Both sides dial at once; exactly one connection survives on each side and it is
/// the one dialed by the side with the lowest fingerprint.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn d_simultaneous_dial_keeps_lowest_fingerprint_dialer() {
    with_timeout(async {
        let mut contended_rounds = 0;
        for round in 0..6 {
            let (mut a, mut b) = pair();
            let (fa, fb) = (a.identity.fingerprint(), b.identity.fingerprint());
            let (pa, pb) = (port(&a), port(&b));
            a.net.dial(&friend("B", pb)).unwrap();
            b.net.dial(&friend("A", pa)).unwrap();

            let (a_side, b_side) = if fa < fb {
                (ConnSide::Dialed, ConnSide::Accepted)
            } else {
                (ConnSide::Accepted, ConnSide::Dialed)
            };
            // Wait for the connections to settle on the expected pair of sides.
            loop {
                if a.net.connection_side("B") == Some(a_side)
                    && b.net.connection_side("A") == Some(b_side)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            // Stay stable (the loser is gone, nobody flips).
            tokio::time::sleep(Duration::from_millis(400)).await;
            assert_eq!(a.net.connection_side("B"), Some(a_side), "round {round}");
            assert_eq!(b.net.connection_side("A"), Some(b_side), "round {round}");

            // Did both connections really coexist at some point?
            let mut connected = 0;
            while let Ok(ev) = a.rx.try_recv() {
                if matches!(ev, NetEvent::Connected { .. }) {
                    connected += 1;
                }
            }
            while let Ok(ev) = b.rx.try_recv() {
                if matches!(ev, NetEvent::Connected { .. }) {
                    connected += 1;
                }
            }
            if connected > 2 {
                contended_rounds += 1;
            }

            // The surviving connection works in both directions.
            a.net.send_control("B", hello("a")).unwrap();
            b.net.send_control("A", hello("b")).unwrap();
            assert_eq!(wait_control(&mut b, "A").await, hello("a"));
            assert_eq!(wait_control(&mut a, "B").await, hello("b"));
            a.net.shutdown().await;
            b.net.shutdown().await;
        }
        assert!(contended_rounds >= 1, "no round exercised two coexisting connections");
    })
    .await;
}

#[tokio::test]
async fn e_reconnects_after_peer_restart_on_same_port() {
    with_timeout(async {
        let id_b = new_identity();
        let mut a = node(new_identity(), vec![entry("B", LOCALHOST, None)], 0);
        let b = node(id_b.clone(), vec![entry("A", LOCALHOST, None)], 0);
        let b_port = port(&b);
        a.net.dial(&friend("B", b_port)).unwrap();
        wait_connected(&mut a, "B").await;
        let pinned = a.net.pinned_fingerprint("B");
        assert_eq!(pinned, Some(id_b.fingerprint()));

        // B goes away cleanly.
        let fa = a.identity.fingerprint();
        b.net.shutdown().await;
        drop(b);
        wait_for(&mut a.rx, |e| matches!(e, NetEvent::Disconnected { .. }).then_some(())).await;
        let (attempt, delay) = wait_for(&mut a.rx, |e| match e {
            NetEvent::Reconnecting { attempt, delay, .. } => Some((attempt, delay)),
            _ => None,
        })
        .await;
        assert_eq!((attempt, delay), (1, Duration::from_secs(1)));

        // B comes back on the same port (retry while the old socket is released).
        let mut b2 = loop {
            let addr = SocketAddr::new(LOCALHOST, b_port);
            let trust = TrustStore::new(vec![entry("A", LOCALHOST, Some(fa))]);
            match Net::bind(addr, &id_b, trust) {
                Ok((net, rx)) => {
                    break Node {
                        net,
                        rx,
                        identity: id_b.clone(),
                    };
                }
                Err(NetError::Bind(_)) => tokio::time::sleep(Duration::from_millis(50)).await,
                Err(e) => panic!("{e}"),
            }
        };
        let (fp, newly_pinned) = wait_connected(&mut a, "B").await;
        assert_eq!(fp, id_b.fingerprint());
        assert!(!newly_pinned, "pin from the first session is kept");
        wait_connected(&mut b2, "A").await;
        a.net.send_control("B", hello("again")).unwrap();
        assert_eq!(wait_control(&mut b2, "A").await, hello("again"));
    })
    .await;
}

#[tokio::test]
async fn f_oversized_datagram_is_an_error_and_not_sent() {
    with_timeout(async {
        let (mut a, mut b) = pair();
        a.net.dial(&friend("B", port(&b))).unwrap();
        wait_connected(&mut a, "B").await;
        wait_connected(&mut b, "A").await;

        let max = a.net.max_datagram_size("B").unwrap();
        assert!(max <= 1200);
        let err = a.net.send_datagram("B", &audio(max + 100)).unwrap_err();
        assert!(
            matches!(err, NetError::Protocol(ProtocolError::TooLarge { .. })),
            "{err:?}"
        );

        // Nothing arrives for the rejected one; a valid one right after does.
        a.net.send_datagram("B", &audio(10)).unwrap();
        let got = wait_for(&mut b.rx, |e| match e {
            NetEvent::Datagram { dg, .. } => Some(dg),
            _ => None,
        })
        .await;
        assert_eq!(got, audio(10), "the first datagram to arrive must be the small one");

        // Not connected: error too.
        assert!(matches!(
            a.net.send_datagram("Nobody", &audio(1)),
            Err(NetError::NotConnected(_))
        ));
    })
    .await;
}
