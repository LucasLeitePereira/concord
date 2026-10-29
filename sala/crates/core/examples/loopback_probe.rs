//! Captures 3 s of the PC's sound through the WASAPI process loopback (Concord's own sound
//! excluded) and prints the level per second. Play something while it runs.
//!
//! `cargo run -p sala-core --example loopback_probe`

#[cfg(windows)]
fn main() {
    use std::time::{Duration, Instant};

    use sala_core::share_audio::FRAME_VALUES;
    use sala_core::voice::level_dbfs;

    let capture = match sala_core::loopback::start() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("process loopback failed: {e}");
            std::process::exit(1);
        }
    };
    let started = Instant::now();
    let (mut frames, mut sum_sq, mut values, mut peak) = (0u32, 0.0f64, 0usize, 0.0f32);
    let mut next_report = Duration::from_secs(1);
    while started.elapsed() < Duration::from_secs(3) {
        let Ok(frame) = capture.frames.recv_timeout(Duration::from_millis(200)) else {
            eprintln!("no frame for 200 ms");
            continue;
        };
        assert_eq!(frame.len(), FRAME_VALUES);
        frames += 1;
        values += frame.len();
        sum_sq += frame.iter().map(|s| f64::from(s * s)).sum::<f64>();
        peak = frame.iter().fold(peak, |m, s| m.max(s.abs()));
        if started.elapsed() >= next_report {
            let rms = (sum_sq / values as f64).sqrt() as f32;
            println!("t={:.0}s frames={frames} rms={:.1} dBFS peak={:.1} dBFS", next_report.as_secs_f32(), level_dbfs(&[rms]), level_dbfs(&[peak]));
            next_report += Duration::from_secs(1);
        }
    }
    let rms = (sum_sq / values.max(1) as f64).sqrt() as f32;
    println!("total: {frames} frames (expect ~150), rms={:.1} dBFS, peak={:.1} dBFS", level_dbfs(&[rms]), level_dbfs(&[peak]));
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Windows only");
}
