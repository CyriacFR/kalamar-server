use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use kalamar_relay::cli::Args;
use kalamar_relay::server;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args = Args::parse();
    let filter = EnvFilter::try_new(&args.log_level).unwrap_or_else(|_| EnvFilter::new("info"));
    use std::io::IsTerminal;
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_ansi(std::io::stdout().is_terminal())
        .init();

    if let Some(path) = &args.new_room_secret {
        // Created exclusively and, on Unix, readable by its owner only from the first instant (roomauth.rs); its
        // folder is created if needed.
        let made = match path.parent().filter(|d| !d.as_os_str().is_empty()) {
            Some(dir) => std::fs::create_dir_all(dir),
            None => Ok(()),
        }
        .and_then(|()| kalamar_relay::roomauth::create_secret_file(path));
        return match made {
            Ok(_) => {
                println!(
                    "wrote {p}: keep it private. Start this relay with --room-secret-file {p}, and give a copy to the service: secret_file of this relay's [[relays]] entry.",
                    p = path.display()
                );
                ExitCode::SUCCESS
            }
            Err(e) => {
                error!("{}: {e}", path.display());
                ExitCode::from(2)
            }
        };
    }
    let cfg = match args.relay_config() {
        Ok(c) => c,
        Err(e) => {
            error!("invalid configuration: {e}");
            return ExitCode::from(2);
        }
    };
    let addr = args.socket_addr();
    let workers = args.worker_count();
    let sockets = match server::bind_with(addr, workers, args.recv_buffer_kb) {
        Ok(s) => s,
        Err(e) => {
            error!(%addr, "cannot bind: {e}");
            return ExitCode::FAILURE;
        }
    };
    let recv_buffer = socket2::SockRef::from(&sockets[0]).recv_buffer_size().unwrap_or(0);
    info!(
        %addr, protocol = 1, workers, sockets = sockets.len(), recv_buffer, max_rooms = cfg.max_rooms,
        max_sessions = cfg.max_sessions, max_members = cfg.max_members, bound = cfg.room_secret.is_some(),
        "relay listening (UDP; a cap of 0 is none)"
    );
    if cfg.room_secret.is_none() {
        warn!("open relay (--open): anyone who knows this address can open rooms on it and use its capacity");
    }
    match server::run_on(
        sockets,
        workers,
        cfg,
        Duration::from_secs(args.stats_interval),
        server::shutdown_signal(),
    )
    .await
    {
        Ok(_) => {
            info!("relay stopped");
            ExitCode::SUCCESS
        }
        Err(e) => {
            error!("relay failed: {e}");
            ExitCode::FAILURE
        }
    }
}
