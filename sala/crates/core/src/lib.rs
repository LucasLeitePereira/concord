//! sala-core library: all business logic lives here; `main.rs` is a thin entry point.

#[cfg(windows)]
pub mod app;
pub mod audio_device;
pub mod calls;
pub mod cli;
pub mod config;
pub mod cues;
pub mod game_detector;
pub mod identity;
#[cfg(windows)]
pub mod ipc;
pub mod jitter;
#[cfg(windows)]
pub mod loopback;
pub mod net;
#[cfg(windows)]
pub mod platform;
pub mod session;
pub mod settings;
pub mod share;
pub mod share_audio;
#[cfg(windows)]
pub mod tailscale;
#[cfg(windows)]
pub mod ui_process;
pub mod voice;
