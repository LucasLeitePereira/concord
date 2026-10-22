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
        use sha2::{Digest, Sha256};
        Fingerprint(Sha256::digest(der).into())
    }

    /// 64 lowercase hex characters.
    pub fn to_hex(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(64);
        for b in self.0 {
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0xf) as usize] as char);
        }
        out
    }

    /// Parses 64 hex characters (case-insensitive).
    pub fn from_hex(s: &str) -> Result<Self, IdentityError> {
        let bad = || IdentityError::InvalidFingerprint(s.to_string());
        if s.len() != 64 || !s.is_ascii() {
            return Err(bad());
        }
        let mut out = [0u8; 32];
        for (i, pair) in s.as_bytes().chunks(2).enumerate() {
            let hi = (pair[0] as char).to_digit(16).ok_or_else(bad)?;
            let lo = (pair[1] as char).to_digit(16).ok_or_else(bad)?;
            out[i] = (hi * 16 + lo) as u8;
        }
        Ok(Fingerprint(out))
    }
}

const CERT_FILE: &str = "identity-cert.der";
const KEY_FILE: &str = "identity-key.der";

/// Writes via a temp file + rename so a crash never leaves a truncated file.
fn write_atomic(path: &Path, data: &[u8], private: bool) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = private;
    std::fs::rename(&tmp, path)
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
        let cert_path = dir.join(CERT_FILE);
        let key_path = dir.join(KEY_FILE);
        if cert_path.is_file() && key_path.is_file() {
            let cert_der = std::fs::read(&cert_path)?;
            let key_der = std::fs::read(&key_path)?;
            if !cert_der.is_empty() && !key_der.is_empty() {
                return Ok(Self { cert_der, key_der });
            }
        }
        std::fs::create_dir_all(dir)?;
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["sala.local".to_string()])
                .map_err(|e| IdentityError::CertGen(e.to_string()))?;
        let cert_der = cert.der().to_vec();
        let key_der = signing_key.serialize_der();
        write_atomic(&key_path, &key_der, true)?;
        write_atomic(&cert_path, &cert_der, false)?;
        Ok(Self { cert_der, key_der })
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
        let addr = addr.to_canonical();
        let Some(friend) = self
            .friends
            .iter_mut()
            .find(|f| f.address.to_canonical() == addr)
        else {
            return PeerDecision::RejectUnknownAddress;
        };
        match friend.fingerprint {
            None => {
                friend.fingerprint = Some(*presented);
                PeerDecision::Accept {
                    friend: friend.name.clone(),
                    newly_pinned: true,
                }
            }
            Some(expected) if expected == *presented => PeerDecision::Accept {
                friend: friend.name.clone(),
                newly_pinned: false,
            },
            Some(expected) => PeerDecision::RejectFingerprintMismatch {
                friend: friend.name.clone(),
                expected,
                presented: *presented,
            },
        }
    }

    /// Explicit user action: forget the pinned fingerprint of `name`.
    /// Returns false if no such friend.
    pub fn reset_fingerprint(&mut self, name: &str) -> bool {
        match self.friends.iter_mut().find(|f| f.name == name) {
            Some(f) => {
                f.fingerprint = None;
                true
            }
            None => false,
        }
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
    // Keep the connection dialed by the side with the smaller fingerprint.
    match side {
        ConnSide::Dialed => local < remote,
        ConnSide::Accepted => remote < local,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(b: u8) -> Fingerprint {
        Fingerprint([b; 32])
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn store() -> TrustStore {
        TrustStore::new(vec![FriendEntry {
            name: "Amigo".into(),
            address: ip("100.1.2.3"),
            fingerprint: None,
        }])
    }

    fn accept(newly_pinned: bool) -> PeerDecision {
        PeerDecision::Accept {
            friend: "Amigo".into(),
            newly_pinned,
        }
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("sala-id-{tag}-{}-{nanos}", std::process::id()))
    }

    #[test]
    fn hex_roundtrip_and_case() {
        let f = Fingerprint::of_cert_der(b"abc");
        let h = f.to_hex();
        assert_eq!(h.len(), 64);
        assert_eq!(h, h.to_lowercase());
        assert_eq!(Fingerprint::from_hex(&h).unwrap(), f);
        assert_eq!(Fingerprint::from_hex(&h.to_uppercase()).unwrap(), f);
    }

    #[test]
    fn sha256_known_vector() {
        assert_eq!(
            Fingerprint::of_cert_der(b"abc").to_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn from_hex_rejects_bad_input() {
        assert!(Fingerprint::from_hex("").is_err());
        assert!(Fingerprint::from_hex(&"a".repeat(63)).is_err());
        assert!(Fingerprint::from_hex(&"a".repeat(66)).is_err());
        assert!(Fingerprint::from_hex(&"g".repeat(64)).is_err());
        assert!(Fingerprint::from_hex(&"é".repeat(32)).is_err());
    }

    #[test]
    fn ordering_matches_hex_ordering() {
        let (a, b) = (fp(0x0f), fp(0xf0));
        assert!(a < b);
        assert!(a.to_hex() < b.to_hex());
    }

    #[test]
    fn identity_is_created_then_reused() {
        let root = temp_dir("reuse");
        let dir = root.join("nested");
        let a = LocalIdentity::load_or_create(&dir).unwrap();
        let b = LocalIdentity::load_or_create(&dir).unwrap();
        assert_eq!(a.cert_der, b.cert_der);
        assert_eq!(a.key_der, b.key_der);
        assert_eq!(a.fingerprint(), b.fingerprint());
        // negative control: a different dir yields a different identity
        let other_dir = temp_dir("other");
        let other = LocalIdentity::load_or_create(&other_dir).unwrap();
        assert_ne!(a.fingerprint(), other.fingerprint());
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(other_dir);
    }

    #[test]
    fn partial_files_are_regenerated() {
        let dir = temp_dir("partial");
        let a = LocalIdentity::load_or_create(&dir).unwrap();
        std::fs::remove_file(dir.join(KEY_FILE)).unwrap();
        let b = LocalIdentity::load_or_create(&dir).unwrap();
        assert_ne!(a.fingerprint(), b.fingerprint());
        let c = LocalIdentity::load_or_create(&dir).unwrap();
        assert_eq!(b.cert_der, c.cert_der);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tofu_pin_then_match_then_mismatch() {
        let mut s = store();
        let a = ip("100.1.2.3");
        assert_eq!(s.check_peer(a, &fp(1)), accept(true));
        assert_eq!(s.friends()[0].fingerprint, Some(fp(1)));
        assert_eq!(s.check_peer(a, &fp(1)), accept(false));
        assert_eq!(
            s.check_peer(a, &fp(2)),
            PeerDecision::RejectFingerprintMismatch {
                friend: "Amigo".into(),
                expected: fp(1),
                presented: fp(2)
            }
        );
        // a mismatch must not overwrite the pin
        assert_eq!(s.friends()[0].fingerprint, Some(fp(1)));
    }

    #[test]
    fn unknown_address_rejected_and_not_pinned() {
        let mut s = store();
        assert_eq!(
            s.check_peer(ip("100.9.9.9"), &fp(1)),
            PeerDecision::RejectUnknownAddress
        );
        assert_eq!(s.friends()[0].fingerprint, None);
    }

    #[test]
    fn ipv4_mapped_ipv6_matches_friend() {
        let mut s = store();
        let mapped = ip("::ffff:100.1.2.3");
        assert_eq!(s.check_peer(mapped, &fp(1)), accept(true));
    }

    #[test]
    fn reset_fingerprint_allows_repin() {
        let mut s = store();
        let a = ip("100.1.2.3");
        s.check_peer(a, &fp(1));
        assert!(!s.reset_fingerprint("Ninguem"));
        assert!(s.reset_fingerprint("Amigo"));
        assert_eq!(s.friends()[0].fingerprint, None);
        assert_eq!(s.check_peer(a, &fp(2)), accept(true));
    }

    #[test]
    fn keep_connection_is_complementary_and_consistent() {
        let (lo, hi) = (fp(1), fp(2));
        // X dialed by lo: lo sees Dialed, hi sees Accepted.
        let x_on_lo = keep_connection(&lo, &hi, ConnSide::Dialed);
        let x_on_hi = keep_connection(&hi, &lo, ConnSide::Accepted);
        assert_eq!(x_on_lo, x_on_hi);
        assert!(
            x_on_lo,
            "connection dialed by the smaller fingerprint is kept"
        );
        // Y dialed by hi: dropped on both machines, complementary to X.
        let y_on_hi = keep_connection(&hi, &lo, ConnSide::Dialed);
        let y_on_lo = keep_connection(&lo, &hi, ConnSide::Accepted);
        assert_eq!(y_on_hi, y_on_lo);
        assert!(!y_on_hi);
    }
}
