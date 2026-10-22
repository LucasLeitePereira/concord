//! Testes ocultos de aceite do par T-004 (não mostrados aos braços).
//! Copiar para sala/crates/core/tests/hidden_t004.rs após as entregas.
use sala_core::jitter::*;

fn pl(i: u32) -> Vec<u8> {
    i.to_le_bytes().to_vec()
}
fn id(p: &[u8]) -> u32 {
    u32::from_le_bytes([p[0], p[1], p[2], p[3]])
}

/// Simula: `arrivals` = (seq, idx, arrival_ms). A cada 20 ms entrega o que chegou e chama pop.
fn simulate(jb: &mut JitterBuffer, mut arrivals: Vec<(u16, u32, u64)>, until_ms: u64) -> Vec<Pop> {
    arrivals.sort_by_key(|a| a.2);
    let mut out = Vec::new();
    let mut k = 0;
    let mut t = 0u64;
    while t <= until_ms {
        while k < arrivals.len() && arrivals[k].2 <= t {
            let (seq, idx, at) = arrivals[k];
            jb.push(seq, idx * 960, pl(idx), at);
            k += 1;
        }
        out.push(jb.pop(t));
        t += FRAME_MS as u64;
    }
    out
}

fn frames(out: &[Pop]) -> Vec<u32> {
    out.iter().filter_map(|p| if let Pop::Frame(b) = p { Some(id(b)) } else { None }).collect()
}

/// Remove os Empty e devolve Some(idx) para Frame, None para Lost.
fn slots(out: &[Pop]) -> Vec<Option<u32>> {
    out.iter()
        .filter_map(|p| match p {
            Pop::Frame(b) => Some(Some(id(b))),
            Pop::Lost => Some(None),
            Pop::Empty => None,
        })
        .collect()
}

#[test]
fn h01_empty_before_anything() {
    let mut jb = JitterBuffer::default();
    assert_eq!(jb.pop(0), Pop::Empty);
    assert_eq!(jb.target_delay_ms(), 40);
    assert!(jb.is_empty());
}

#[test]
fn h02_playout_starts_after_target_delay() {
    let mut jb = JitterBuffer::default();
    jb.push(0, 0, pl(0), 100);
    assert_eq!(jb.pop(120), Pop::Empty);
    assert_eq!(jb.pop(140), Pop::Frame(pl(0)));
}

#[test]
fn h03_in_order_regular_no_loss() {
    let mut jb = JitterBuffer::default();
    let arr = (0..120u32).map(|i| (i as u16, i, i as u64 * 20)).collect();
    let out = simulate(&mut jb, arr, 2000);
    let s = slots(&out);
    assert_eq!(&s[..90], &(0..90).map(Some).collect::<Vec<_>>()[..]);
    assert_eq!(jb.target_delay_ms(), 40, "jitter zero deve manter o mínimo");
}

#[test]
fn h04_reorder_within_window() {
    let mut jb = JitterBuffer::default();
    let mut arr: Vec<(u16, u32, u64)> = (0..120u32).map(|i| (i as u16, i, i as u64 * 20)).collect();
    for i in (1..110).step_by(4) {
        let (a, b) = (arr[i].2, arr[i + 1].2);
        arr[i].2 = b;
        arr[i + 1].2 = a;
    }
    let out = simulate(&mut jb, arr, 2400);
    let f = frames(&out);
    assert_eq!(&f[..100], &(0..100).collect::<Vec<_>>()[..]);
    assert!(!slots(&out)[..100].contains(&None), "reordenação dentro da janela não pode gerar Lost");
}

#[test]
fn h05_missing_frame_yields_lost_in_its_slot() {
    let mut jb = JitterBuffer::default();
    let arr = (0..60u32).filter(|&i| i != 10).map(|i| (i as u16, i, i as u64 * 20)).collect();
    let out = simulate(&mut jb, arr, 1000);
    let s = slots(&out);
    let p9 = s.iter().position(|x| *x == Some(9)).expect("frame 9");
    assert_eq!(s[p9 + 1], None, "slot do 10 deve ser Lost");
    assert_eq!(s[p9 + 2], Some(11));
    assert!(jb.stats().lost >= 1);
}

#[test]
fn h06_duplicate_played_once() {
    let mut jb = JitterBuffer::default();
    let mut arr: Vec<(u16, u32, u64)> = (0..60u32).map(|i| (i as u16, i, i as u64 * 20)).collect();
    arr.push((5, 5, 5 * 20 + 5));
    let out = simulate(&mut jb, arr, 1000);
    assert_eq!(frames(&out).iter().filter(|&&x| x == 5).count(), 1);
    assert_eq!(jb.stats().duplicates, 1);
}

#[test]
fn h07_late_frame_dropped_not_played() {
    let mut jb = JitterBuffer::default();
    let arr = (0..60u32).map(|i| (i as u16, i, if i == 10 { 10 * 20 + 400 } else { i as u64 * 20 })).collect();
    let out = simulate(&mut jb, arr, 1000);
    assert!(!frames(&out).contains(&10), "quadro atrasado não pode tocar fora de ordem");
    assert_eq!(jb.stats().late_dropped, 1);
    let s = slots(&out);
    let p9 = s.iter().position(|x| *x == Some(9)).unwrap();
    assert_eq!(s[p9 + 1], None);
}

#[test]
fn h08_sequence_wraparound() {
    let mut jb = JitterBuffer::default();
    let start: u16 = 65_530;
    let arr = (0..40u32).map(|i| (start.wrapping_add(i as u16), i, i as u64 * 20)).collect();
    let out = simulate(&mut jb, arr, 700);
    let s = slots(&out);
    assert_eq!(&s[..30], &(0..30).map(Some).collect::<Vec<_>>()[..]);
}

#[test]
fn h09_target_always_within_bounds_under_heavy_jitter() {
    let mut jb = JitterBuffer::default();
    let mut x: u64 = 0x9E3779B97F4A7C15;
    let mut rnd = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let mut arr = Vec::new();
    for i in 0..1500u32 {
        let jitter = rnd() % 300;
        if rnd() % 20 == 0 {
            continue; // 5% perda
        }
        arr.push((i as u16, i, i as u64 * 20 + jitter));
    }
    arr.sort_by_key(|a| a.2);
    let mut k = 0;
    let mut t = 0u64;
    let mut raised = false;
    while t <= 31_000 {
        while k < arr.len() && arr[k].2 <= t {
            jb.push(arr[k].0, arr[k].1 * 960, pl(arr[k].1), arr[k].2);
            k += 1;
            let d = jb.target_delay_ms();
            assert!((40..=120).contains(&d), "target {d} fora de [40,120]");
        }
        let _ = jb.pop(t);
        let d = jb.target_delay_ms();
        assert!((40..=120).contains(&d), "target {d} fora de [40,120]");
        raised |= d > 40;
        t += 20;
    }
    assert!(raised, "jitter alto deve elevar o target acima do mínimo");
}

#[test]
fn h10_overflow_bounded() {
    let mut jb = JitterBuffer::default();
    for i in 0..20u32 {
        jb.push(i as u16, i * 960, pl(i), 0);
    }
    assert!(jb.len() <= (120 / FRAME_MS + 1) as usize, "len {} acima da capacidade", jb.len());
    assert!(jb.stats().overflow_dropped >= 13);
}

#[test]
fn h11_custom_bounds_respected() {
    let mut jb = JitterBuffer::new(60, 80);
    assert_eq!(jb.target_delay_ms(), 60);
    let arr = (0..300u32).map(|i| (i as u16, i, i as u64 * 20 + if i % 2 == 0 { 0 } else { 150 })).collect();
    let _ = simulate(&mut jb, arr, 6500);
    assert!((60..=80).contains(&jb.target_delay_ms()));
}
