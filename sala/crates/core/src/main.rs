use std::net::{Ipv4Addr, SocketAddr};

use sala_core::cli::CliArgs;
use sala_core::config::{Config, Profile};
use sala_core::identity::LocalIdentity;
use sala_core::session::{AudioSource, Session};
use sala_protocol::PROTOCOL_VERSION;

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
    let config = Config::load_or_create(&profile).map_err(|e| e.to_string())?;
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
    let source = if args.synthetic_audio { AudioSource::Synthetic } else { AudioSource::Device };
    let session = Session::start(profile, config, &identity, bind_addr, source)?;
    let _ = tokio::signal::ctrl_c().await;
    session.shutdown().await;
    Ok(())
}
