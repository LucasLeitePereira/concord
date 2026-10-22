use sala_core::cli::CliArgs;
use sala_core::config::{Config, Profile};

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let args = CliArgs::parse(std::env::args().skip(1))?;
    let profile = Profile::resolve(args.profile.as_deref()).map_err(|e| e.to_string())?;
    let config = Config::load_or_create(&profile).map_err(|e| e.to_string())?;
    println!(
        "sala-core {} (protocol v{})",
        env!("CARGO_PKG_VERSION"),
        sala_protocol::PROTOCOL_VERSION
    );
    println!("name: {}", config.name);
    println!("port: {}", config.port);
    println!("data_dir: {}", profile.data_dir.display());
    println!("friends: {}", config.friends.len());
    Ok(())
}
