//! T-035a: the viewer can be opened, closed and opened again in the same process (`winit`
//! allows a single event loop, so the UI thread must outlive every window). Needs a real window
//! and a GPU: `cargo test -p sala-video --test viewer_reopen -- --ignored`.
#![cfg(windows)]

use std::sync::Arc;
use std::time::Duration;

use sala_video::viewer::{self, Incoming};

#[test]
#[ignore = "opens real windows (needs a desktop and a GPU)"]
fn the_viewer_opens_closes_and_reopens_three_times() {
    let (_tx, mut rx) = std::sync::mpsc::channel::<Incoming>();
    for round in 1..=3 {
        let handle = viewer::open(rx, Arc::new(|_| {})).unwrap_or_else(|e| panic!("round {round}: {e}"));
        assert!(handle.is_open(), "round {round}");
        std::thread::sleep(Duration::from_millis(300));
        rx = handle.close().unwrap_or_else(|| panic!("round {round}: the input channel must come back"));
    }
}
