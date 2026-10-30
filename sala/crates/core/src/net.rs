//! P2P QUIC connection (`peer-connection`, D-007).
//!
//! Both peers listen; whoever starts the call dials. Each connection has one
//! bidirectional control stream (opened by the dialer) plus media datagrams.
//! Trust is decided after the TLS handshake by `TrustStore::check_peer`.
//! Logs never include chat content.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::server::danger::{ClientCertVerifier, ClientCertVerified};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};
use rustls_pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use sala_protocol::{
    ControlMessage, DEFAULT_MAX_DATAGRAM, FrameDecoder, MAX_CONTROL_FRAME, MediaDatagram,
    ProtocolError,
};
use tokio::sync::{mpsc, watch};

use crate::config::{Config, Friend};
use crate::identity::{
    ConnSide, Fingerprint, FriendEntry, LocalIdentity, PeerDecision, TrustStore, keep_connection,
};

/// ALPN protocol identifier.
pub const ALPN: &[u8] = b"sala/1";
/// Interval for QUIC keep-alive and `ControlMessage::Heartbeat`.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);
/// A connection with no traffic for this long is considered dead.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(6);
/// Reconnection backoff: 1, 2, 4, 8, 15, 15... seconds.
pub const BACKOFF_MAX: Duration = Duration::from_secs(15);
/// A connection that lived at least this long resets the backoff.
const STABLE_AFTER: Duration = Duration::from_secs(2);
const CONTROL_SETUP_TIMEOUT: Duration = Duration::from_secs(5);
const SERVER_NAME: &str = "sala.local";

// Application close codes.
const CODE_NORMAL: u32 = 0;
const CODE_REJECTED: u32 = 1;
const CODE_MISMATCH: u32 = 2;
const CODE_SUPERSEDED: u32 = 3;
const CODE_PROTOCOL: u32 = 4;

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("tls setup: {0}")]
    Tls(String),
    #[error("bind: {0}")]
    Bind(#[from] std::io::Error),
    #[error("unknown friend: {0}")]
    UnknownFriend(String),
    #[error("friend already exists: {0}")]
    DuplicateFriend(String),
    #[error("invalid friend address: {0}")]
    InvalidAddress(String),
    #[error("not connected to {0}")]
    NotConnected(String),
    #[error("peer does not accept datagrams")]
    DatagramsUnsupported,
    #[error("protocol: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("send datagram: {0}")]
    SendDatagram(String),
}

/// Snapshot of the QUIC connection counters (a subset of `quinn::ConnectionStats`).
#[derive(Debug, Clone, Copy)]
pub struct TransportStats {
    pub rtt_ms: f32,
    pub cwnd: u64,
    pub congestion_events: u64,
    pub lost_packets: u64,
    pub sent_packets: u64,
    /// UDP datagrams (QUIC packets) sent / received by the connection.
    pub udp_tx: u64,
    pub udp_rx: u64,
    /// QUIC DATAGRAM frames sent / received.
    pub datagram_frames_tx: u64,
    pub datagram_frames_rx: u64,
    /// Room left in the connection's outgoing datagram buffer, in bytes.
    pub send_buffer_space: usize,
    /// Largest datagram the connection accepts right now (the path MTU moves it).
    pub max_datagram: usize,
    pub mtu: u16,
    /// Times QUIC concluded that big packets vanish and fell back to the minimum MTU.
    pub black_holes: u64,
}

/// Events emitted to the application (UI / main loop).
#[derive(Debug, Clone)]
pub enum NetEvent {
    /// A connection to `friend` became the active one. `newly_pinned`: the
    /// fingerprint was pinned (TOFU) and the caller should persist it.
    Connected {
        friend: String,
        fingerprint: Fingerprint,
        newly_pinned: bool,
    },
    Control {
        friend: String,
        msg: ControlMessage,
    },
    Datagram {
        friend: String,
        dg: MediaDatagram,
    },
    Disconnected {
        friend: String,
    },
    Reconnecting {
        friend: String,
        attempt: u32,
        delay: Duration,
    },
    /// Inbound connection refused (address is not a friend).
    Rejected {
        addr: SocketAddr,
        reason: String,
    },
    FingerprintMismatch {
        friend: String,
        expected: Fingerprint,
        presented: Fingerprint,
    },
}

/// Backoff delay for the n-th consecutive failure (n >= 1): 1, 2, 4, 8, 15, 15...
pub fn backoff_delay(attempt: u32) -> Duration {
    let secs = 1u64 << attempt.saturating_sub(1).min(4);
    Duration::from_secs(secs).min(BACKOFF_MAX)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionFailure {
    Timeout,
    HandshakeRefused,
    FingerprintMismatch,
    Io,
}

impl ConnectionFailure {
    fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::HandshakeRefused => "handshake refused",
            Self::FingerprintMismatch => "fingerprint mismatch",
            Self::Io => "I/O error",
        }
    }
}

fn classify_connection_failure(message: &str) -> ConnectionFailure {
    let message = message.to_ascii_lowercase();
    if message.contains("timedout") || message.contains("timed out") || message.contains("timeout") {
        ConnectionFailure::Timeout
    } else if message.contains("fingerprint mismatch") {
        ConnectionFailure::FingerprintMismatch
    } else if message.contains("connectionclosed") || message.contains("connection closed")
        || message.contains("applicationclosed") || message.contains("refused")
        || message.contains("handshake") || message.contains("versionmismatch")
    {
        ConnectionFailure::HandshakeRefused
    } else {
        ConnectionFailure::Io
    }
}

fn log_failure_if_changed(last: &mut Option<ConnectionFailure>, failure: ConnectionFailure, friend: &str) {
    if should_log_failure(last, failure) {
        tracing::warn!(%friend, reason = failure.as_str(), "outgoing connection failed");
    }
}

#[cfg(test)]
mod failure_classification_tests {
    use super::*;

    #[test]
    fn classifies_quic_failure_reasons() {
        assert_eq!(classify_connection_failure("connection timed out"), ConnectionFailure::Timeout);
        assert_eq!(classify_connection_failure("connection closed: fingerprint mismatch"), ConnectionFailure::FingerprintMismatch);
        assert_eq!(classify_connection_failure("ApplicationClosed: not a friend"), ConnectionFailure::HandshakeRefused);
        assert_eq!(classify_connection_failure("handshake refused by peer"), ConnectionFailure::HandshakeRefused);
        assert_eq!(classify_connection_failure("UDP socket I/O error"), ConnectionFailure::Io);
    }

    #[test]
    fn reports_first_failure_and_only_changes() {
        let mut last = None;
        assert!(should_log_failure(&mut last, ConnectionFailure::Timeout));
        assert!(!should_log_failure(&mut last, ConnectionFailure::Timeout));
        assert!(should_log_failure(&mut last, ConnectionFailure::Io));
    }
}

fn should_log_failure(last: &mut Option<ConnectionFailure>, failure: ConnectionFailure) -> bool {
    if *last == Some(failure) { false } else { *last = Some(failure); true }
}

/// Builds the trust store from the config's friends (invalid entries are skipped with a warning).
pub fn trust_store_from_config(config: &Config) -> TrustStore {
    let mut entries = Vec::new();
    for f in &config.friends {
        let Ok(address) = f.address.parse::<IpAddr>() else {
            tracing::warn!(friend = %f.name, "skipping friend with invalid address");
            continue;
        };
        let fingerprint = match f.fingerprint.as_deref().map(Fingerprint::from_hex) {
            None => None,
            Some(Ok(fp)) => Some(fp),
            Some(Err(_)) => {
                tracing::warn!(friend = %f.name, "ignoring invalid fingerprint in config");
                None
            }
        };
        entries.push(FriendEntry {
            name: f.name.clone(),
            address,
            fingerprint,
        });
    }
    TrustStore::new(entries)
}

// ---------------------------------------------------------------- TLS

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Accepts any certificate; only checks the handshake signature. Trust is
/// decided after the handshake (D-007).
#[derive(Debug)]
struct AnyCert {
    algs: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for AnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algs)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

impl ClientCertVerifier for AnyCert {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algs)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

fn transport() -> Arc<quinn::TransportConfig> {
    let mut t = quinn::TransportConfig::default();
    t.keep_alive_interval(Some(HEARTBEAT_INTERVAL));
    t.max_idle_timeout(Some(
        quinn::IdleTimeout::try_from(IDLE_TIMEOUT).expect("idle timeout fits"),
    ));
    t.max_concurrent_bidi_streams(1u8.into());
    t.max_concurrent_uni_streams(0u8.into());
    Arc::new(t)
}

/// Kernel buffer size asked for the QUIC socket. The Windows default receive buffer is 64 KB,
/// which a keyframe burst overflows as soon as the receiver loses the CPU for a moment.
const SOCKET_BUFFER_BYTES: i32 = 4 << 20;

/// Raises SO_RCVBUF and SO_SNDBUF of the endpoint socket and logs what the OS granted.
#[cfg(windows)]
fn tune_socket_buffers(socket: &std::net::UdpSocket) {
    use std::os::windows::io::AsRawSocket;
    use windows_sys::Win32::Networking::WinSock::{SO_RCVBUF, SO_SNDBUF, SOL_SOCKET, getsockopt, setsockopt};

    let raw = socket.as_raw_socket() as usize;
    let mut effective = [0i32; 2];
    for (slot, opt) in effective.iter_mut().zip([SO_RCVBUF, SO_SNDBUF]) {
        // SAFETY: `raw` is the live socket owned by `socket`; the option buffers are valid i32s.
        unsafe {
            let want = SOCKET_BUFFER_BYTES;
            if setsockopt(raw, SOL_SOCKET, opt, (&raw const want).cast(), 4) != 0 {
                tracing::warn!(option = opt, "cannot resize the socket buffer");
            }
            let mut len = 4i32;
            let mut got = 0i32;
            if getsockopt(raw, SOL_SOCKET, opt, (&raw mut got).cast(), &mut len) == 0 {
                *slot = got;
            }
        }
    }
    tracing::info!(recv_buffer = effective[0], send_buffer = effective[1], "UDP socket buffers");
}

#[cfg(not(windows))]
fn tune_socket_buffers(_socket: &std::net::UdpSocket) {}

fn build_configs(
    identity: &LocalIdentity,
) -> Result<(quinn::ServerConfig, quinn::ClientConfig), NetError> {
    let tls_err = |e: &dyn std::fmt::Display| NetError::Tls(e.to_string());
    let prov = provider();
    let verifier = Arc::new(AnyCert {
        algs: prov.signature_verification_algorithms,
    });
    let chain = vec![CertificateDer::from(identity.cert_der.clone())];
    let key = || rustls_pki_types::PrivateKeyDer::from(PrivatePkcs8KeyDer::from(identity.key_der.clone()));

    let mut server = rustls::ServerConfig::builder_with_provider(prov.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| tls_err(&e))?
        .with_client_cert_verifier(verifier.clone())
        .with_single_cert(chain.clone(), key())
        .map_err(|e| tls_err(&e))?;
    server.alpn_protocols = vec![ALPN.to_vec()];

    let mut client = rustls::ClientConfig::builder_with_provider(prov)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| tls_err(&e))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(chain, key())
        .map_err(|e| tls_err(&e))?;
    client.alpn_protocols = vec![ALPN.to_vec()];

    let mut server_cfg = quinn::ServerConfig::with_crypto(Arc::new(
        QuicServerConfig::try_from(server).map_err(|e| tls_err(&e))?,
    ));
    server_cfg.transport_config(transport());
    let mut client_cfg = quinn::ClientConfig::new(Arc::new(
        QuicClientConfig::try_from(client).map_err(|e| tls_err(&e))?,
    ));
    client_cfg.transport_config(transport());
    Ok((server_cfg, client_cfg))
}

// ---------------------------------------------------------------- state

struct Active {
    id: u64,
    conn: quinn::Connection,
    side: ConnSide,
    tx: mpsc::UnboundedSender<ControlMessage>,
    since: Instant,
}

struct Inner {
    endpoint: quinn::Endpoint,
    local_fp: Fingerprint,
    trust: Mutex<TrustStore>,
    conns: Mutex<HashMap<String, Active>>,
    /// Friends pinned (TOFU) whose pin was not yet reported through `Connected`.
    unreported_pins: Mutex<HashSet<String>>,
    last_inbound_failures: Mutex<HashMap<IpAddr, String>>,
    last_outbound_refusals: Mutex<HashMap<String, ConnectionFailure>>,
    /// Friends being dialed, with the token of their supervisor (a removed and
    /// re-added friend gets a new supervisor; the old one notices and ends).
    dialing: Mutex<HashMap<String, u64>>,
    events: mpsc::UnboundedSender<NetEvent>,
    shutdown: watch::Sender<bool>,
    next_id: AtomicU64,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Inner {
    fn emit(&self, ev: NetEvent) {
        let _ = self.events.send(ev);
    }

    fn is_connected(&self, friend: &str) -> bool {
        lock(&self.conns).contains_key(friend)
    }

    fn is_friend_ip(&self, ip: IpAddr) -> bool {
        let ip = ip.to_canonical();
        lock(&self.trust)
            .friends()
            .iter()
            .any(|f| f.address.to_canonical() == ip)
    }
}

/// Handle to the QUIC endpoint. Cloning is cheap.
#[derive(Clone)]
pub struct Net {
    inner: Arc<Inner>,
}

impl Net {
    /// Binds the UDP port and starts accepting connections. Must be called
    /// inside a tokio runtime. Use port 0 for an ephemeral port.
    pub fn bind(
        addr: SocketAddr,
        identity: &LocalIdentity,
        trust: TrustStore,
    ) -> Result<(Net, mpsc::UnboundedReceiver<NetEvent>), NetError> {
        let (server_cfg, client_cfg) = build_configs(identity)?;
        // Same as `Endpoint::server`, but with room in the kernel buffers (see `tune_socket_buffers`).
        let socket = std::net::UdpSocket::bind(addr)?;
        tune_socket_buffers(&socket);
        let runtime = quinn::default_runtime().ok_or_else(|| std::io::Error::other("no async runtime found"))?;
        let mut endpoint = quinn::Endpoint::new(quinn::EndpointConfig::default(), Some(server_cfg), socket, runtime)?;
        endpoint.set_default_client_config(client_cfg);
        let (events, rx) = mpsc::unbounded_channel();
        let (shutdown, _) = watch::channel(false);
        let inner = Arc::new(Inner {
            endpoint,
            local_fp: identity.fingerprint(),
            trust: Mutex::new(trust),
            conns: Mutex::new(HashMap::new()),
            unreported_pins: Mutex::new(HashSet::new()),
            last_inbound_failures: Mutex::new(HashMap::new()),
            last_outbound_refusals: Mutex::new(HashMap::new()),
            dialing: Mutex::new(HashMap::new()),
            events,
            shutdown,
            next_id: AtomicU64::new(1),
        });
        tokio::spawn(accept_loop(inner.clone()));
        Ok((Net { inner }, rx))
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.inner
            .endpoint
            .local_addr()
            .expect("bound endpoint has a local address")
    }

    pub fn local_fingerprint(&self) -> Fingerprint {
        self.inner.local_fp
    }

    /// Starts (and keeps alive, with backoff) the outgoing connection to `friend`.
    /// Calling it again for a friend that is already being dialed does nothing.
    pub fn dial(&self, friend: &Friend) -> Result<(), NetError> {
        let ip: IpAddr = friend
            .address
            .parse()
            .map_err(|_| NetError::InvalidAddress(friend.address.clone()))?;
        if !lock(&self.inner.trust)
            .friends()
            .iter()
            .any(|f| f.name == friend.name)
        {
            return Err(NetError::UnknownFriend(friend.name.clone()));
        }
        let token = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut dialing = lock(&self.inner.dialing);
            if dialing.contains_key(&friend.name) {
                return Ok(());
            }
            dialing.insert(friend.name.clone(), token);
        }
        let target = SocketAddr::new(ip, friend.effective_port());
        tokio::spawn(supervise(self.inner.clone(), friend.name.clone(), target, token));
        Ok(())
    }

    /// Adds `friend` to the allowlist and starts dialing it. Errors on a duplicate
    /// name or an invalid address (nothing changes then).
    pub fn add_friend(&self, friend: &Friend) -> Result<(), NetError> {
        let address: IpAddr = friend
            .address
            .parse()
            .map_err(|_| NetError::InvalidAddress(friend.address.clone()))?;
        let fingerprint = friend.fingerprint.as_deref().and_then(|h| Fingerprint::from_hex(h).ok());
        let entry = FriendEntry { name: friend.name.clone(), address, fingerprint };
        if !lock(&self.inner.trust).add_friend(entry) {
            return Err(NetError::DuplicateFriend(friend.name.clone()));
        }
        self.dial(friend)
    }

    /// Removes `friend` from the allowlist, stops dialing it and closes its connection
    /// (the usual `Disconnected` event follows when it was connected).
    pub fn remove_friend(&self, name: &str) {
        lock(&self.inner.trust).remove_friend(name);
        lock(&self.inner.dialing).remove(name);
        lock(&self.inner.unreported_pins).remove(name);
        let conn = lock(&self.inner.conns).get(name).map(|a| a.conn.clone());
        if let Some(c) = conn {
            c.close(CODE_NORMAL.into(), b"friend removed");
        }
    }

    /// Forgets the pinned fingerprint of `name` (the next connection pins again).
    /// False if there is no such friend.
    pub fn reset_fingerprint(&self, name: &str) -> bool {
        lock(&self.inner.trust).reset_fingerprint(name)
    }

    /// Round-trip time estimate of the active connection to `friend`.
    pub fn rtt(&self, friend: &str) -> Option<Duration> {
        lock(&self.inner.conns).get(friend).map(|a| a.conn.rtt())
    }

    /// How long the active connection to `friend` has been up.
    pub fn connected_for(&self, friend: &str) -> Option<Duration> {
        lock(&self.inner.conns).get(friend).map(|a| a.since.elapsed())
    }

    /// Queues a control message on the active connection to `friend`.
    pub fn send_control(&self, friend: &str, msg: ControlMessage) -> Result<(), NetError> {
        // Reject oversized frames here so callers (e.g. chat history) never record
        // a message that the writer would silently drop.
        msg.encode_frame().map_err(NetError::Protocol)?;
        let conns = lock(&self.inner.conns);
        let active = conns
            .get(friend)
            .ok_or_else(|| NetError::NotConnected(friend.to_string()))?;
        active
            .tx
            .send(msg)
            .map_err(|_| NetError::NotConnected(friend.to_string()))
    }

    /// Sends a media datagram. Fails (without sending anything) when the encoded
    /// size exceeds the connection's maximum datagram size (`inv-datagram-max-size`).
    pub fn send_datagram(&self, friend: &str, dg: &MediaDatagram) -> Result<(), NetError> {
        let conn = self.connection(friend)?;
        let max = conn
            .max_datagram_size()
            .ok_or(NetError::DatagramsUnsupported)?
            .min(DEFAULT_MAX_DATAGRAM);
        let bytes = dg.encode(max)?;
        conn.send_datagram(bytes.into())
            .map_err(|e| NetError::SendDatagram(e.to_string()))
    }

    /// QUIC counters of the active connection to `friend` (for the `share stats` log).
    pub fn transport_stats(&self, friend: &str) -> Option<TransportStats> {
        let conn = self.connection(friend).ok()?;
        let s = conn.stats();
        Some(TransportStats {
            rtt_ms: conn.rtt().as_secs_f32() * 1000.0,
            cwnd: s.path.cwnd,
            congestion_events: s.path.congestion_events,
            lost_packets: s.path.lost_packets,
            sent_packets: s.path.sent_packets,
            udp_tx: s.udp_tx.datagrams,
            udp_rx: s.udp_rx.datagrams,
            datagram_frames_tx: s.frame_tx.datagram,
            datagram_frames_rx: s.frame_rx.datagram,
            send_buffer_space: conn.datagram_send_buffer_space(),
            max_datagram: conn.max_datagram_size().unwrap_or(0),
            mtu: s.path.current_mtu,
            black_holes: s.path.black_holes_detected,
        })
    }

    /// Largest datagram currently allowed towards `friend`.
    pub fn max_datagram_size(&self, friend: &str) -> Option<usize> {
        let conn = self.connection(friend).ok()?;
        Some(conn.max_datagram_size()?.min(DEFAULT_MAX_DATAGRAM))
    }

    fn connection(&self, friend: &str) -> Result<quinn::Connection, NetError> {
        lock(&self.inner.conns)
            .get(friend)
            .map(|a| a.conn.clone())
            .ok_or_else(|| NetError::NotConnected(friend.to_string()))
    }

    pub fn is_connected(&self, friend: &str) -> bool {
        self.inner.is_connected(friend)
    }

    /// Which side started the active connection to `friend`.
    pub fn connection_side(&self, friend: &str) -> Option<ConnSide> {
        lock(&self.inner.conns).get(friend).map(|a| a.side)
    }

    /// Fingerprint currently pinned for `friend` in the trust store.
    pub fn pinned_fingerprint(&self, friend: &str) -> Option<Fingerprint> {
        lock(&self.inner.trust)
            .friends()
            .iter()
            .find(|f| f.name == friend)
            .and_then(|f| f.fingerprint)
    }

    /// Clean shutdown: sends `Bye`, closes every connection and the endpoint.
    pub async fn shutdown(&self) {
        let _ = self.inner.shutdown.send(true);
        let deadline = Instant::now() + Duration::from_secs(2);
        while !lock(&self.inner.conns).is_empty() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        self.inner.endpoint.close(CODE_NORMAL.into(), b"shutdown");
        let _ = tokio::time::timeout(Duration::from_secs(2), self.inner.endpoint.wait_idle()).await;
    }
}

// ---------------------------------------------------------------- tasks

async fn accept_loop(inner: Arc<Inner>) {
    let mut shutdown = inner.shutdown.subscribe();
    loop {
        let incoming = tokio::select! {
            i = inner.endpoint.accept() => match i { Some(i) => i, None => return },
            _ = shutdown.changed() => return,
        };
        let addr = incoming.remote_address();
        if !inner.is_friend_ip(addr.ip()) {
            log_inbound_refusal(&inner, addr, "address is not a friend");
            incoming.refuse();
            inner.emit(NetEvent::Rejected {
                addr,
                reason: "address is not a friend".into(),
            });
            continue;
        }
        let inner = inner.clone();
        tokio::spawn(async move {
            match incoming.await {
                Ok(conn) => establish(inner, conn, ConnSide::Accepted).await,
                Err(e) => {
                    let detail = format!("{e:?}: {e}");
                    let reason = classify_connection_failure(&detail);
                    log_inbound_refusal(&inner, addr, reason.as_str());
                }
            }
        });
    }
}

/// Keeps one outgoing connection to a friend alive, with backoff (D-007).
async fn supervise(inner: Arc<Inner>, friend: String, target: SocketAddr, token: u64) {
    let mut shutdown = inner.shutdown.subscribe();
    let mut attempt: u32 = 0;
    let mut connected_since: Option<Instant> = None;
    let mut ready_to_dial = true;
    let mut last_failure = None;
    loop {
        if *shutdown.borrow() {
            break;
        }
        // The friend was removed (and maybe re-added with another supervisor).
        if lock(&inner.dialing).get(&friend) != Some(&token) {
            return;
        }
        if inner.is_connected(&friend) {
            last_failure = None;
            connected_since.get_or_insert_with(Instant::now);
            ready_to_dial = false;
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(100)) => {}
                _ = shutdown.changed() => {}
            }
            continue;
        }
        if let Some(since) = connected_since.take() {
            // Connection lost.
            if since.elapsed() >= STABLE_AFTER {
                attempt = 0;
            }
            ready_to_dial = false;
        }
        if !ready_to_dial {
            attempt += 1;
            let delay = backoff_delay(attempt);
            inner.emit(NetEvent::Reconnecting {
                friend: friend.clone(),
                attempt,
                delay,
            });
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = shutdown.changed() => break,
            }
            ready_to_dial = true;
            continue; // re-check: an inbound connection may have arrived meanwhile
        }
        // Dial.
        tracing::info!(%friend, %target, "dialing");
        let connecting = match inner.endpoint.connect(target, SERVER_NAME) {
            Ok(c) => c,
            Err(e) => {
                let detail = format!("{e:?}: {e}");
                let reason = classify_connection_failure(&detail);
                log_failure_if_changed(&mut last_failure, reason, &friend);
                ready_to_dial = false;
                continue;
            }
        };
        let result = tokio::select! {
            r = connecting => r,
            _ = shutdown.changed() => break,
        };
        match result {
            Ok(conn) => {
                establish(inner.clone(), conn, ConnSide::Dialed).await;
                if !inner.is_connected(&friend) {
                    // Rejected or dropped right away.
                    ready_to_dial = false;
                } else {
                    connected_since = Some(Instant::now());
                }
            }
            Err(e) => {
                let detail = format!("{e:?}: {e}");
                let reason = classify_connection_failure(&detail);
                log_failure_if_changed(&mut last_failure, reason, &friend);
                ready_to_dial = false;
            }
        }
    }
    let mut dialing = lock(&inner.dialing);
    if dialing.get(&friend) == Some(&token) {
        dialing.remove(&friend);
    }
}

fn log_inbound_refusal(inner: &Inner, addr: SocketAddr, reason: &str) {
    let mut failures = lock(&inner.last_inbound_failures);
    if failures.get(&addr.ip()).is_none_or(|last| last != reason) {
        tracing::warn!(%addr, reason, "inbound connection refused");
        failures.insert(addr.ip(), reason.to_string());
    }
}

fn log_outbound_refusal(inner: &Inner, friend: &str, addr: SocketAddr, failure: ConnectionFailure) {
    let mut failures = lock(&inner.last_outbound_refusals);
    if failures.get(friend) != Some(&failure) {
        tracing::warn!(%friend, %addr, reason = failure.as_str(), "outgoing connection refused");
        failures.insert(friend.to_string(), failure);
    }
}

fn peer_fingerprint(conn: &quinn::Connection) -> Option<Fingerprint> {
    let identity = conn.peer_identity()?;
    let certs = identity.downcast::<Vec<CertificateDer<'static>>>().ok()?;
    certs.first().map(|c| Fingerprint::of_cert_der(c.as_ref()))
}

/// Post-handshake: trust decision, control stream, duplicate resolution, then runs
/// the connection until it ends (for accepted connections) or spawns the run task
/// (for dialed ones, so the supervisor can keep watching).
async fn establish(inner: Arc<Inner>, conn: quinn::Connection, side: ConnSide) {
    let addr = conn.remote_address();
    let Some(peer_fp) = peer_fingerprint(&conn) else {
        conn.close(CODE_PROTOCOL.into(), b"no certificate");
        return;
    };
    let decision = lock(&inner.trust).check_peer(addr.ip(), &peer_fp);
    let (friend, newly_pinned) = match decision {
        PeerDecision::Accept { friend, newly_pinned } => (friend, newly_pinned),
        PeerDecision::RejectUnknownAddress => {
            match side {
                ConnSide::Accepted => log_inbound_refusal(&inner, addr, "address is not a friend"),
                ConnSide::Dialed => log_outbound_refusal(&inner, "unknown friend", addr, ConnectionFailure::HandshakeRefused),
            }
            conn.close(CODE_REJECTED.into(), b"not a friend");
            inner.emit(NetEvent::Rejected {
                addr,
                reason: "address is not a friend".into(),
            });
            return;
        }
        PeerDecision::RejectFingerprintMismatch {
            friend,
            expected,
            presented,
        } => {
            match side {
                ConnSide::Accepted => log_inbound_refusal(&inner, addr, "fingerprint mismatch"),
                ConnSide::Dialed => log_outbound_refusal(&inner, &friend, addr, ConnectionFailure::FingerprintMismatch),
            }
            conn.close(CODE_MISMATCH.into(), b"fingerprint mismatch");
            inner.emit(NetEvent::FingerprintMismatch {
                friend,
                expected,
                presented,
            });
            return;
        }
    };
    if newly_pinned {
        lock(&inner.unreported_pins).insert(friend.clone());
    }

    // Control stream: opened by the dialer (its first frame is a Heartbeat so the
    // acceptor sees the stream), accepted by the other side, which answers with a
    // Heartbeat. The dialer only proceeds after reading that first frame, so a peer
    // that refuses us after the handshake (e.g. fingerprint mismatch) never makes the
    // dialer report `Connected`.
    let setup = tokio::time::timeout(CONTROL_SETUP_TIMEOUT, async {
        let heartbeat = ControlMessage::Heartbeat
            .encode_frame()
            .map_err(|e| e.to_string())?;
        let mut decoder = FrameDecoder::new(MAX_CONTROL_FRAME);
        match side {
            ConnSide::Dialed => {
                let (mut send, mut recv) = conn.open_bi().await.map_err(|e| e.to_string())?;
                send.write_all(&heartbeat).await.map_err(|e| e.to_string())?;
                let mut buf = vec![0u8; 4096];
                loop {
                    if let Some(first) = decoder.next_message().map_err(|e| e.to_string())? {
                        if !matches!(first, ControlMessage::Heartbeat) {
                            return Err("unexpected first control frame".to_string());
                        }
                        break;
                    }
                    match recv.read(&mut buf).await.map_err(|e| e.to_string())? {
                        Some(n) => decoder.push(&buf[..n]),
                        None => return Err("control stream closed during setup".to_string()),
                    }
                }
                Ok::<_, String>((send, recv, decoder))
            }
            ConnSide::Accepted => {
                let (mut send, recv) = conn.accept_bi().await.map_err(|e| e.to_string())?;
                send.write_all(&heartbeat).await.map_err(|e| e.to_string())?;
                Ok((send, recv, decoder))
            }
        }
    })
    .await;
    let (send, recv, decoder) = match setup {
        Ok(Ok(streams)) => streams,
        Ok(Err(e)) => {
            tracing::debug!(%friend, "control stream setup failed: {e}");
            conn.close(CODE_PROTOCOL.into(), b"no control stream");
            return;
        }
        Err(_) => {
            tracing::warn!(%friend, "control stream setup timed out");
            conn.close(CODE_PROTOCOL.into(), b"control stream timeout");
            return;
        }
    };

    // Register, resolving simultaneous connections (inv-simultaneous-conn-lowest-fp).
    let id = inner.next_id.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::unbounded_channel();
    {
        let mut conns = lock(&inner.conns);
        if let Some(old) = conns.get(&friend) {
            // Same side: the new one is a fresh attempt (peer restarted), it wins.
            // Different sides: both dialed; keep the one chosen by the fingerprint rule.
            let new_wins = old.side == side || keep_connection(&inner.local_fp, &peer_fp, side);
            if new_wins {
                old.conn.close(CODE_SUPERSEDED.into(), b"superseded");
            } else {
                tracing::info!(%friend, "duplicate connection dropped (lowest-fingerprint rule)");
                conn.close(CODE_SUPERSEDED.into(), b"superseded");
                return;
            }
        }
        conns.insert(
            friend.clone(),
            Active {
                id,
                conn: conn.clone(),
                side,
                tx,
                since: Instant::now(),
            },
        );
    }
    let newly_pinned = lock(&inner.unreported_pins).remove(&friend) || newly_pinned;
    tracing::info!(%friend, ?side, newly_pinned, "connected");
    inner.emit(NetEvent::Connected {
        friend: friend.clone(),
        fingerprint: peer_fp,
        newly_pinned,
    });
    let task = run_connection(inner, conn, friend, id, (send, recv, decoder), rx);
    match side {
        ConnSide::Dialed => {
            tokio::spawn(task);
        }
        ConnSide::Accepted => task.await,
    }
}

async fn run_connection(
    inner: Arc<Inner>,
    conn: quinn::Connection,
    friend: String,
    id: u64,
    (send, recv, decoder): (quinn::SendStream, quinn::RecvStream, FrameDecoder),
    rx: mpsc::UnboundedReceiver<ControlMessage>,
) {
    let shutdown = inner.shutdown.subscribe();
    tokio::select! {
        _ = write_loop(send, rx, shutdown) => {}
        _ = read_loop(recv, decoder, &inner, &friend) => {}
        _ = datagram_loop(&conn, &inner, &friend) => {}
        _ = conn.closed() => {}
    }
    conn.close(CODE_NORMAL.into(), b"bye");
    let was_active = {
        let mut conns = lock(&inner.conns);
        if conns.get(&friend).is_some_and(|a| a.id == id) {
            conns.remove(&friend);
            true
        } else {
            false
        }
    };
    if was_active {
        tracing::info!(%friend, "disconnected");
        inner.emit(NetEvent::Disconnected { friend });
    }
}

async fn write_frame(send: &mut quinn::SendStream, msg: &ControlMessage) -> bool {
    match msg.encode_frame() {
        Ok(frame) => send.write_all(&frame).await.is_ok(),
        Err(e) => {
            tracing::warn!("cannot encode control message: {e}");
            true // drop this message, keep the stream
        }
    }
}

async fn write_loop(
    mut send: quinn::SendStream,
    mut rx: mpsc::UnboundedReceiver<ControlMessage>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut beat = tokio::time::interval(HEARTBEAT_INTERVAL);
    beat.tick().await;
    loop {
        tokio::select! {
            msg = rx.recv() => {
                let Some(msg) = msg else { return };
                if !write_frame(&mut send, &msg).await { return; }
            }
            _ = beat.tick() => {
                if !write_frame(&mut send, &ControlMessage::Heartbeat).await { return; }
            }
            _ = shutdown.changed() => {
                if write_frame(&mut send, &ControlMessage::Bye).await && send.finish().is_ok() {
                    let _ = tokio::time::timeout(Duration::from_millis(500), send.stopped()).await;
                }
                return;
            }
        }
    }
}

async fn read_loop(
    mut recv: quinn::RecvStream,
    mut decoder: FrameDecoder,
    inner: &Inner,
    friend: &str,
) {
    let mut buf = vec![0u8; 4096];
    loop {
        // Drain first: setup may have left complete frames in the decoder.
        let msgs = drain_frames(&mut decoder, friend);
        let mut done = msgs.end;
        for msg in msgs.messages {
            let bye = matches!(msg, ControlMessage::Bye);
            inner.emit(NetEvent::Control {
                friend: friend.to_string(),
                msg,
            });
            done |= bye;
            if bye {
                break;
            }
        }
        if done {
            return;
        }
        match recv.read(&mut buf).await {
            Ok(Some(n)) => decoder.push(&buf[..n]),
            Ok(None) | Err(_) => return,
        }
    }
}

/// Complete control messages found in the decoder, in order.
struct Drained {
    messages: Vec<ControlMessage>,
    /// The stream is unusable (a frame over the size limit): stop reading.
    end: bool,
}

/// Takes every complete frame out of `decoder`. Heartbeats are dropped. A frame whose
/// content does not decode (a variant a newer peer added, D-024) is skipped: the decoder
/// already consumed it, so the stream stays in sync. Only an oversized frame ends it.
fn drain_frames(decoder: &mut FrameDecoder, friend: &str) -> Drained {
    let mut messages = Vec::new();
    loop {
        match decoder.next_message() {
            Ok(Some(ControlMessage::Heartbeat)) => {}
            Ok(Some(msg)) => messages.push(msg),
            Ok(None) => return Drained { messages, end: false },
            Err(ProtocolError::Decode(e)) => {
                tracing::warn!(%friend, "ignoring an unknown or malformed control frame: {e}");
            }
            Err(e) => {
                tracing::warn!(%friend, "invalid control frame: {e}");
                return Drained { messages, end: true };
            }
        }
    }
}

#[cfg(test)]
mod drain_tests {
    use super::*;

    fn frame_with_variant(index: u32) -> Vec<u8> {
        let mut payload = index.to_le_bytes().to_vec();
        payload.extend_from_slice(&1u64.to_le_bytes());
        let mut f = (payload.len() as u32).to_be_bytes().to_vec();
        f.extend_from_slice(&payload);
        f
    }

    #[test]
    fn an_unknown_variant_is_skipped_and_the_messages_around_it_arrive() {
        let mut d = FrameDecoder::new(MAX_CONTROL_FRAME);
        d.push(&ControlMessage::Bye.encode_frame().unwrap());
        d.push(&frame_with_variant(99)); // what a future version might send
        d.push(&ControlMessage::ShareStop.encode_frame().unwrap());
        let got = drain_frames(&mut d, "x");
        assert_eq!(got.messages, vec![ControlMessage::Bye, ControlMessage::ShareStop]);
        assert!(!got.end, "the stream must survive an unknown variant");
        // A ring from this version decodes (control for the test above).
        let mut d = FrameDecoder::new(MAX_CONTROL_FRAME);
        d.push(&frame_with_variant(8));
        assert_eq!(drain_frames(&mut d, "x").messages, vec![ControlMessage::Ring { call_id: 1 }]);
    }

    #[test]
    fn an_oversized_frame_still_ends_the_stream() {
        let mut d = FrameDecoder::new(100);
        d.push(&1000u32.to_be_bytes());
        let got = drain_frames(&mut d, "x");
        assert!(got.end && got.messages.is_empty());
    }
}

async fn datagram_loop(conn: &quinn::Connection, inner: &Inner, friend: &str) {
    while let Ok(bytes) = conn.read_datagram().await {
        match MediaDatagram::decode(&bytes) {
            Ok(dg) => inner.emit(NetEvent::Datagram {
                friend: friend.to_string(),
                dg,
            }),
            Err(e) => tracing::debug!(%friend, "dropping malformed datagram: {e}"),
        }
    }
}
