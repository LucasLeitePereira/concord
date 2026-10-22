//! Local TLS identity, friend trust store (fingerprint pinning + address
//! allowlist) and the duplicate-connection tie-break rule.
//!
//! API skeleton fixed by the architect. Implement the `todo!()` bodies; do not
//! change public signatures, type names, variants or public fields. Private
//! items may be added. The QUIC/rustls wiring is a separate task: this module
//! is pure logic plus certificate persistence.

use std::net::IpAddr;
use std::path::Path;

/// Errors from identity handling.
#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("certificate generation failed: {0}")]
    CertGen(String),
    #[error("invalid fingerprint: {0}")]
    InvalidFingerprint(String),
}

/// SHA-256 of a DER certificate. Ordering is lexicographic over the bytes
/// (equivalent to comparing the lowercase hex strings).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Fingerprint(pub [u8; 32]);

impl Fingerprint {
    /// Fingerprint of a DER-encoded certificate.
    pub fn of_cert_der(der: &[u8]) -> Self {
        let _ = der;
        todo!()
    }

    /// 64 lowercase hex characters.
    pub fn to_hex(&self) -> String {
        todo!()
    }

    /// Parses 64 hex characters (case-insensitive).
    pub fn from_hex(s: &str) -> Result<Self, IdentityError> {
        let _ = s;
        todo!()
    }
}

/// This instance's self-signed certificate and private key.
#[derive(Debug, Clone)]
pub struct LocalIdentity {
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
}

impl LocalIdentity {
    /// Loads the identity stored in `dir`, or generates a new self-signed
    /// certificate on first run and saves it there (creating `dir` if needed).
    /// Subsequent calls with the same `dir` return the same identity.
    pub fn load_or_create(dir: &Path) -> Result<Self, IdentityError> {
        let _ = dir;
        todo!()
    }

    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint::of_cert_der(&self.cert_der)
    }
}

/// A configured friend as seen by the trust store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FriendEntry {
    pub name: String,
    pub address: IpAddr,
    /// `None` until the first successful connection.
    pub fingerprint: Option<Fingerprint>,
}

/// Outcome of checking an incoming or outgoing peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerDecision {
    /// Connection allowed. `newly_pinned` is true when the fingerprint was
    /// recorded by this call (the caller must persist the config).
    Accept { friend: String, newly_pinned: bool },
    /// Remote address is not a configured friend.
    RejectUnknownAddress,
    /// Address belongs to `friend` but the certificate differs from the pinned one.
    /// The UI must warn the user.
    RejectFingerprintMismatch {
        friend: String,
        expected: Fingerprint,
        presented: Fingerprint,
    },
}

/// Friends allowlist + fingerprint pinning state.
#[derive(Debug, Clone)]
pub struct TrustStore {
    friends: Vec<FriendEntry>,
}

impl TrustStore {
    pub fn new(friends: Vec<FriendEntry>) -> Self {
        Self { friends }
    }

    pub fn friends(&self) -> &[FriendEntry] {
        &self.friends
    }

    /// Decides whether a peer at `addr` presenting `presented` may connect,
    /// updating pinning state when appropriate.
    pub fn check_peer(&mut self, addr: IpAddr, presented: &Fingerprint) -> PeerDecision {
        let _ = (addr, presented);
        todo!()
    }

    /// Explicit user action: forget the pinned fingerprint of `name`.
    /// Returns false if no such friend.
    pub fn reset_fingerprint(&mut self, name: &str) -> bool {
        let _ = name;
        todo!()
    }
}

/// Which side opened a given connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnSide {
    /// We dialed the peer.
    Dialed,
    /// The peer dialed us.
    Accepted,
}

/// When two connections with the same friend exist at once (both dialed),
/// returns whether the connection described by (`local`, `remote`, `side`)
/// must be kept. Must give complementary answers for the two connections and
/// the same answer on both machines for the same connection.
pub fn keep_connection(local: &Fingerprint, remote: &Fingerprint, side: ConnSide) -> bool {
    let _ = (local, remote, side);
    todo!()
}
