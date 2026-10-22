use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use sala_core::audio_device::{AudioInput, Sink};
use sala_core::cli::CliArgs;
use sala_core::config::{Config, Profile};
use sala_core::identity::LocalIdentity;
use sala_core::net::{Net, NetEvent, trust_store_from_config};
use sala_core::voice::{SineSource, VoiceEngine};
use sala_protocol::{
    AudioStream, Capabilities, ControlMessage, MediaDatagram, PROTOCOL_VERSION, max_audio_payload,
};
use tokio::sync::mpsc;
use tokio::time::{MissedTickBehavior, interval};

fn main() {
    tracing_subscriber::fmt().with_target(false).init();
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(2);
    }
}

#[tokio::main]
async fn run() -> Result<(), String> {
    let args = CliArgs::parse(std::env::args().skip(1))?;
    let profile = Profile::resolve(args.profile.as_deref()).map_err(|e| e.to_string())?;
    let mut config = Config::load_or_create(&profile).map_err(|e| e.to_string())?;
    let identity = LocalIdentity::load_or_create(&profile.data_dir).map_err(|e| e.to_string())?;
    tracing::info!(
        "sala-core {} (protocol v{}) name={} port={} friends={} fingerprint={}",
        env!("CARGO_PKG_VERSION"),
        PROTOCOL_VERSION,
        config.name,
        config.port,
        config.friends.len(),
        identity.fingerprint().to_hex()
    );

    let bind_addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, config.port));
    let (net, mut events) = Net::bind(bind_addr, &identity, trust_store_from_config(&config))
        .map_err(|e| format!("cannot listen on {bind_addr}: {e}"))?;
    for friend in &config.friends {
        if let Err(e) = net.dial(friend) {
            tracing::warn!(friend = %friend.name, "cannot dial: {e}");
        }
    }

    // Voice: microphone (device or synthetic) -> engine -> Net; Net -> engine -> sink.
    let mut engine = VoiceEngine::new(&config.audio).map_err(|e| format!("opus: {e}"))?;
    let (cap_tx, mut cap_rx) = mpsc::channel::<Vec<f32>>(16);
    let _input = if args.synthetic_audio {
        let tx = cap_tx.clone();
        tokio::spawn(async move {
            let mut src = SineSource::standard();
            let mut tick = interval(Duration::from_millis(20));
            tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                if tx.try_send(src.next_frame()).is_err() && tx.is_closed() {
                    break;
                }
            }
        });
        None
    } else {
        match AudioInput::start(&config.audio.input_device, cap_tx.clone()) {
            Ok(i) => Some(i),
            Err(e) => {
                tracing::warn!("no audio input ({e}); not transmitting voice");
                None
            }
        }
    };
    let sink = if args.synthetic_audio { Sink::Null } else { Sink::open(&config.audio.output_device) };
    tracing::info!(synthetic = args.synthetic_audio, null_sink = sink.is_null(), "voice ready");
    let started = Instant::now();
    let mut playout = interval(Duration::from_millis(20));
    playout.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut stats_tick = interval(Duration::from_secs(2));
    let mut send_errors: u64 = 0;

    loop {
        let event = tokio::select! {
            e = events.recv() => match e { Some(e) => e, None => break },
            Some(mut pcm) = cap_rx.recv() => {
                match engine.process_capture(&mut pcm) {
                    Ok(Some(f)) => {
                        let peers: Vec<String> = engine.peer_names().map(str::to_string).collect();
                        for peer in peers {
                            let Some(max) = net.max_datagram_size(&peer) else { continue };
                            if f.payload.len() > max_audio_payload(max) {
                                send_errors += 1;
                                continue;
                            }
                            let dg = MediaDatagram::Audio {
                                stream: AudioStream::Voice,
                                seq: f.seq,
                                timestamp: f.timestamp,
                                payload: f.payload.clone(),
                            };
                            if net.send_datagram(&peer, &dg).is_err() {
                                send_errors += 1;
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(e) => tracing::warn!("opus encode failed: {e}"),
                }
                continue;
            }
            _ = playout.tick() => {
                let now_ms = started.elapsed().as_millis() as u64;
                sink.play(engine.playout_tick(now_ms));
                continue;
            }
            _ = stats_tick.tick() => {
                let s = engine.stats();
                tracing::info!(sent = s.sent, skipped = s.skipped, send_errors, "voice tx");
                let names: Vec<String> = engine.peer_names().map(str::to_string).collect();
                for n in names {
                    if let Some(p) = engine.peer_stats(&n) {
                        tracing::info!(
                            friend = %n,
                            received = p.jitter.received,
                            played = p.jitter.played,
                            lost = p.jitter.lost,
                            late = p.jitter.late_dropped,
                            plc = p.plc_frames,
                            fec = p.fec_frames,
                            "voice rx"
                        );
                    }
                }
                continue;
            }
            _ = tokio::signal::ctrl_c() => break,
        };
        match event {
            NetEvent::Connected { friend, fingerprint, newly_pinned } => {
                tracing::info!(%friend, fingerprint = %fingerprint.to_hex(), newly_pinned, "event: connected");
                if let Err(e) = engine.add_peer(&friend) {
                    tracing::warn!(%friend, "cannot start voice: {e}");
                }
                let hello = ControlMessage::Hello {
                    name: config.name.clone(),
                    app_version: env!("CARGO_PKG_VERSION").to_string(),
                    protocol_version: PROTOCOL_VERSION,
                    capabilities: Capabilities::default(),
                };
                let status = ControlMessage::Status {
                    muted: false,
                    deafened: false,
                    game_mode: false,
                    sharing: false,
                };
                for msg in [hello, status] {
                    if let Err(e) = net.send_control(&friend, msg) {
                        tracing::warn!(%friend, "cannot send: {e}");
                    }
                }
                if newly_pinned
                    && let Some(f) = config.friends.iter_mut().find(|f| f.name == friend)
                {
                    f.fingerprint = Some(fingerprint.to_hex());
                    match config.save(&profile) {
                        Ok(()) => tracing::info!(%friend, "fingerprint pinned (TOFU) and saved"),
                        Err(e) => tracing::error!(%friend, "cannot save pinned fingerprint: {e}"),
                    }
                }
            }
            NetEvent::Control { friend, msg } => match msg {
                ControlMessage::Hello { name, app_version, protocol_version, .. } => {
                    tracing::info!(%friend, %name, %app_version, protocol_version, "event: Hello");
                }
                ControlMessage::Status { muted, deafened, game_mode, sharing } => {
                    tracing::info!(%friend, muted, deafened, game_mode, sharing, "event: Status");
                }
                // Never log chat text.
                ControlMessage::Chat { id, .. } => tracing::info!(%friend, id, "event: Chat"),
                other => tracing::info!(%friend, "event: control {other:?}"),
            },
            NetEvent::Datagram { friend, dg } => match dg {
                MediaDatagram::Audio { stream: AudioStream::Voice, seq, timestamp, payload } => {
                    let now_ms = started.elapsed().as_millis() as u64;
                    engine.push_datagram(&friend, seq, timestamp, payload, now_ms);
                }
                _ => tracing::trace!(%friend, "event: datagram"),
            },
            NetEvent::Disconnected { friend } => {
                engine.remove_peer(&friend);
                tracing::info!(%friend, "event: disconnected");
            }
            NetEvent::Reconnecting { friend, attempt, delay } => {
                tracing::info!(%friend, attempt, ?delay, "event: reconnecting");
            }
            NetEvent::Rejected { addr, reason } => {
                tracing::warn!(%addr, %reason, "event: rejected");
            }
            NetEvent::FingerprintMismatch { friend, expected, presented } => {
                tracing::warn!(
                    %friend,
                    expected = %expected.to_hex(),
                    presented = %presented.to_hex(),
                    "event: FINGERPRINT MISMATCH, connection refused"
                );
            }
        }
    }
    net.shutdown().await;
    Ok(())
}
