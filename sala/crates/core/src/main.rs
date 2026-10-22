use std::net::{Ipv4Addr, SocketAddr};

use sala_core::cli::CliArgs;
use sala_core::config::{Config, Profile};
use sala_core::identity::LocalIdentity;
use sala_core::net::{Net, NetEvent, trust_store_from_config};
use sala_protocol::{Capabilities, ControlMessage, PROTOCOL_VERSION};

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

    loop {
        let event = tokio::select! {
            e = events.recv() => match e { Some(e) => e, None => break },
            _ = tokio::signal::ctrl_c() => break,
        };
        match event {
            NetEvent::Connected { friend, fingerprint, newly_pinned } => {
                tracing::info!(%friend, fingerprint = %fingerprint.to_hex(), newly_pinned, "event: connected");
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
            NetEvent::Datagram { friend, .. } => tracing::trace!(%friend, "event: datagram"),
            NetEvent::Disconnected { friend } => tracing::info!(%friend, "event: disconnected"),
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
