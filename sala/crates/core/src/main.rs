// Release: no console window; logs go to the file (debug keeps the console for tests).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use sala_core::cli::CliArgs;
use sala_core::config::{Config, Profile};
use sala_core::identity::LocalIdentity;
use sala_core::session::AudioSource;
use sala_protocol::PROTOCOL_VERSION;

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(2);
    }
}

/// Logs to `<data_dir>/logs/sala-core.log.<date>` (daily rotation, 14 files) and
/// to stderr. The returned guard flushes the file on drop.
fn init_logging(profile: &Profile) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::fmt::writer::MakeWriterExt;
    // The appender lists the directory when pruning old files: create it first.
    let _ = std::fs::create_dir_all(profile.data_dir.join("logs"));
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("sala-core.log")
        .max_log_files(14)
        .build(profile.data_dir.join("logs"));
    match appender {
        Ok(a) => {
            let (file, guard) = tracing_appender::non_blocking(a);
            tracing_subscriber::fmt()
                .with_target(false)
                .with_ansi(false)
                .with_timer(tracing_subscriber::fmt::time::LocalTime::rfc_3339())
                .with_writer(file.and(std::io::stderr))
                .init();
            Some(guard)
        }
        Err(e) => {
            tracing_subscriber::fmt()
                .with_target(false)
                .with_timer(tracing_subscriber::fmt::time::LocalTime::rfc_3339())
                .init();
            tracing::warn!("cannot write logs to disk: {e}");
            None
        }
    }
}

#[tokio::main]
async fn run() -> Result<(), String> {
    let args = CliArgs::parse(std::env::args().skip(1))?;
    let profile = Profile::resolve(args.profile.as_deref()).map_err(|e| e.to_string())?;
    let _log_guard = init_logging(&profile);
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
    let source = if args.synthetic_audio { AudioSource::Synthetic } else { AudioSource::Device };
    match sala_core::app::run(args.profile.as_deref(), profile, config, &identity, source).await {
        Ok(()) => Ok(()),
        Err(sala_core::app::AppError::AlreadyRunning) => {
            // Second instance: ask the running one to show its UI and leave.
            tracing::info!("already running; asking the running instance to open the UI");
            sala_core::app::notify_running_instance(args.profile.as_deref())
                .await
                .map_err(|e| format!("cannot reach the running instance: {e}"))
        }
        Err(e) => Err(e.to_string()),
    }
}
