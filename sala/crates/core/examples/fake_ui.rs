//! Stand-in for `sala-ui` in tests: connects to `--pipe <name>`, sends `GetState`,
//! exits on `Shutdown` (or never, with `--ignore-shutdown`).

#[cfg(windows)]
#[tokio::main(flavor = "current_thread")]
async fn main() {
    use sala_core::ipc::IpcClient;
    use sala_protocol::{CoreToUi, UiToCore};

    let args: Vec<String> = std::env::args().collect();
    let pipe = args.iter().position(|a| a == "--pipe").and_then(|i| args.get(i + 1)).cloned();
    let ignore = args.iter().any(|a| a == "--ignore-shutdown");
    let Some(pipe) = pipe else {
        eprintln!("usage: fake_ui --pipe <name> [--ignore-shutdown]");
        std::process::exit(2);
    };
    let mut client = match IpcClient::connect(&pipe).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("connect failed: {e}");
            std::process::exit(1);
        }
    };
    if client.send(&UiToCore::GetState).await.is_err() {
        std::process::exit(1);
    }
    loop {
        match client.recv().await {
            Ok(Some(CoreToUi::Shutdown)) if !ignore => break,
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => {
                if ignore {
                    tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                }
                break;
            }
        }
    }
}

#[cfg(not(windows))]
fn main() {}
