// Release: no console window; logs go to the file (debug keeps the console for tests).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use sala_core::cli::CliArgs;
use sala_core::config::{Config, MigrationOutcome, Profile};
use sala_core::identity::LocalIdentity;
use sala_core::session::AudioSource;
use sala_protocol::PROTOCOL_VERSION;

fn main() {
    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: cannot start the runtime: {e}");
            std::process::exit(2);
        }
    };
    let result = rt.block_on(run());
    // A hung audio open (WASAPI) must not keep the process alive after "Quit" (T-029):
    // don't wait for blocking tasks; detached threads die with the process.
    rt.shutdown_timeout(std::time::Duration::from_secs(1));
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(2);
    }
}

/// Prints on the console. The release build has no console subsystem, so it attaches to
/// the parent's console and writes to `CONOUT$` (std handles are null in that case).
fn print_to_console(text: &str) {
    #[cfg(windows)]
    {
        use std::io::Write;
        use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS, GetStdHandle, STD_OUTPUT_HANDLE};
        // SAFETY: plain Win32 calls without pointers.
        let has_stdout = unsafe { !GetStdHandle(STD_OUTPUT_HANDLE).is_null() };
        if has_stdout {
            // Debug build or redirected output: ordinary stdout works.
            print!("{text}");
            return;
        }
        unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
        if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open("CONOUT$")
            && f.write_all(text.as_bytes()).is_ok()
        {
            return;
        }
    }
    print!("{text}");
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

async fn run() -> Result<(), String> {
    let args = CliArgs::parse(std::env::args().skip(1))?;
    if args.probe {
        let report = tokio::task::spawn_blocking(|| sala_video::report().summary())
            .await
            .map_err(|e| e.to_string())?;
        print_to_console(&report);
        return Ok(());
    }
    let (profile, migration) = Profile::resolve_with_outcome(args.profile.as_deref()).map_err(|e| e.to_string())?;
    let _log_guard = init_logging(&profile);
    match migration {
        MigrationOutcome::Migrated => tracing::info!("migrated legacy Sala data directory to Concord"),
        MigrationOutcome::Fallback => tracing::warn!("could not migrate Sala data directory; using it for this run and will retry next startup"),
        MigrationOutcome::BothExist => tracing::warn!("both Sala and Concord data directories exist; using Concord and leaving Sala untouched"),
        MigrationOutcome::None => {}
    }
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
    // Probe the video hardware once now (cached for every Hello); log what was found.
    match tokio::task::spawn_blocking(|| sala_video::report().summary()).await {
        Ok(summary) => tracing::info!("video probe:\n{summary}"),
        Err(e) => tracing::warn!("video probe failed: {e}"),
    }
    let source = if args.synthetic_audio { AudioSource::SyntheticMic } else { AudioSource::Device };
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
