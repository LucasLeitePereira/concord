//! Testes ocultos de aceite do par T-006 (não mostrados aos braços).
//! Copiar para sala/crates/core/tests/hidden_t006.rs após as entregas.
use sala_core::identity::*;
use std::net::IpAddr;
use std::path::PathBuf;

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sala-h006-{tag}-{}-{:?}", std::process::id(), std::thread::current().id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}
fn fp(b: u8) -> Fingerprint {
    Fingerprint([b; 32])
}
fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}
fn store() -> TrustStore {
    TrustStore::new(vec![
        FriendEntry { name: "Pedro".into(), address: ip("100.101.12.7"), fingerprint: None },
        FriendEntry { name: "Camila".into(), address: ip("100.64.0.9"), fingerprint: Some(fp(9)) },
    ])
}

#[test]
fn h01_identity_persisted_and_stable() {
    let d = tmpdir("a");
    let a = LocalIdentity::load_or_create(&d.join("sub")).unwrap();
    let b = LocalIdentity::load_or_create(&d.join("sub")).unwrap();
    assert_eq!(a.fingerprint(), b.fingerprint());
    assert_eq!(a.cert_der, b.cert_der);
    let c = LocalIdentity::load_or_create(&tmpdir("b")).unwrap();
    assert_ne!(a.fingerprint(), c.fingerprint(), "cada instalação gera certificado próprio");
    assert!(!a.key_der.is_empty());
}

#[test]
fn h02_fingerprint_hex_roundtrip_and_sha256_shape() {
    let id = LocalIdentity::load_or_create(&tmpdir("c")).unwrap();
    let f = id.fingerprint();
    let h = f.to_hex();
    assert_eq!(h.len(), 64);
    assert!(h.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    assert_eq!(Fingerprint::from_hex(&h).unwrap(), f);
    assert_eq!(Fingerprint::from_hex(&h.to_uppercase()).unwrap(), f);
    assert!(Fingerprint::from_hex("abc").is_err());
    assert!(Fingerprint::from_hex(&"zz".repeat(32)).is_err());
    // determinístico
    assert_eq!(Fingerprint::of_cert_der(&id.cert_der), f);
    assert_ne!(Fingerprint::of_cert_der(b"x"), Fingerprint::of_cert_der(b"y"));
}

#[test]
fn h03_tofu_pins_on_first_contact() {
    let mut s = store();
    assert_eq!(s.check_peer(ip("100.101.12.7"), &fp(1)), PeerDecision::Accept { friend: "Pedro".into(), newly_pinned: true });
    assert_eq!(s.friends()[0].fingerprint, Some(fp(1)));
    assert_eq!(s.check_peer(ip("100.101.12.7"), &fp(1)), PeerDecision::Accept { friend: "Pedro".into(), newly_pinned: false });
}

#[test]
fn h04_mismatch_rejected_and_pin_kept() {
    let mut s = store();
    let d = s.check_peer(ip("100.64.0.9"), &fp(7));
    assert_eq!(d, PeerDecision::RejectFingerprintMismatch { friend: "Camila".into(), expected: fp(9), presented: fp(7) });
    assert_eq!(s.friends()[1].fingerprint, Some(fp(9)), "recusa não pode sobrescrever o pin");
    // continua recusando
    assert!(matches!(s.check_peer(ip("100.64.0.9"), &fp(7)), PeerDecision::RejectFingerprintMismatch { .. }));
}

#[test]
fn h05_reset_allows_new_pin() {
    let mut s = store();
    assert!(s.reset_fingerprint("Camila"));
    assert_eq!(s.friends()[1].fingerprint, None);
    assert_eq!(s.check_peer(ip("100.64.0.9"), &fp(7)), PeerDecision::Accept { friend: "Camila".into(), newly_pinned: true });
    assert!(!s.reset_fingerprint("Ninguém"));
}

#[test]
fn h06_unknown_address_rejected_without_side_effects() {
    let mut s = store();
    let before = s.friends().to_vec();
    assert_eq!(s.check_peer(ip("100.1.1.1"), &fp(1)), PeerDecision::RejectUnknownAddress);
    assert_eq!(s.check_peer(ip("100.1.1.1"), &fp(9)), PeerDecision::RejectUnknownAddress);
    assert_eq!(s.friends(), &before[..]);
}

#[test]
fn h07_keep_connection_lowest_fingerprint_dialer() {
    let (a, b) = (fp(0x10), fp(0x20)); // a < b
    // conexão discada por A (menor): mantida
    assert!(keep_connection(&a, &b, ConnSide::Dialed));
    assert!(keep_connection(&b, &a, ConnSide::Accepted));
    // conexão discada por B (maior): fechada
    assert!(!keep_connection(&b, &a, ConnSide::Dialed));
    assert!(!keep_connection(&a, &b, ConnSide::Accepted));
}

#[test]
fn h08_keep_connection_consistent_property() {
    let mut x: u64 = 0xDEADBEEFCAFEF00D;
    for _ in 0..500 {
        let mut p = [0u8; 32];
        let mut q = [0u8; 32];
        for i in 0..32 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            p[i] = x as u8;
            q[i] = if i < 31 { p[i] } else { (x >> 8) as u8 }; // muitas vezes só o último byte difere
        }
        let (l, r) = (Fingerprint(p), Fingerprint(q));
        if l == r {
            continue;
        }
        // mesma conexão vista dos dois lados => mesma decisão
        assert_eq!(keep_connection(&l, &r, ConnSide::Dialed), keep_connection(&r, &l, ConnSide::Accepted));
        // exatamente uma das duas conexões sobrevive
        assert_ne!(keep_connection(&l, &r, ConnSide::Dialed), keep_connection(&l, &r, ConnSide::Accepted));
        // sobrevive a discada por quem tem o menor fingerprint
        assert_eq!(keep_connection(&l, &r, ConnSide::Dialed), l < r);
    }
}
