//! `jam-server serve` keeps the jam running; `jam-server code` prints the
//! code listeners paste into Spotifast.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use jam_core::wire::Limits;
use jam_server::DataDir;

#[derive(Parser)]
#[command(version, about = "Keeps a Spotifast jam running for its listeners")]
struct Cli {
    /// Where the password, the certificate and the jam are kept.
    #[arg(long, global = true, default_value = "data")]
    data: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the jam until stopped.
    Serve {
        #[arg(long, default_value = "0.0.0.0:4070")]
        listen: SocketAddr,
    },
    /// Print the server code. Listeners enter it beside the server's
    /// address. It holds the password: share it privately.
    Code,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let data = DataDir::new(cli.data);
    match cli.command {
        Command::Code => match data.server_code() {
            Ok(code) => {
                println!("{code}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("jam-server: {error}");
                ExitCode::FAILURE
            }
        },
        Command::Serve { listen } => {
            let runtime = tokio::runtime::Runtime::new().expect("cannot start the runtime");
            runtime.block_on(serve(data, listen))
        }
    }
}

async fn serve(data: DataDir, listen: SocketAddr) -> ExitCode {
    let secret = match data.load_or_create_secret() {
        Ok(secret) => secret,
        Err(error) => {
            eprintln!("jam-server: cannot read or create the password: {error}");
            return ExitCode::FAILURE;
        }
    };
    let (tls, fingerprint) = match data.load_tls() {
        Ok(tls) => tls,
        Err(error) => {
            eprintln!("jam-server: {error}");
            return ExitCode::FAILURE;
        }
    };
    let running =
        match jam_server::start(listen, tls, secret, Some(data.state()), Limits::default()).await {
            Ok(running) => running,
            Err(error) => {
                eprintln!("jam-server: cannot listen at {listen}: {error}");
                return ExitCode::FAILURE;
            }
        };
    // The fingerprint is public; the password never goes to the log.
    let hex: String = fingerprint
        .0
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    eprintln!(
        "jam-server: listening at {}, certificate SHA-256 {hex}",
        running.address
    );
    eprintln!("jam-server: run `jam-server code` for the server code");
    stopped().await;
    eprintln!("jam-server: stopping");
    running.stop().await;
    ExitCode::SUCCESS
}

/// Ctrl+C, or the SIGTERM a container or service manager sends.
async fn stopped() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate()).expect("cannot watch for SIGTERM");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
