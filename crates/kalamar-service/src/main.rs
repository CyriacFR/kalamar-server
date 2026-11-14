use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use kalamar_service::admin::{self, AdminCmd};
use kalamar_service::clock::{Clock, ManualClock, SystemClock};
use kalamar_service::config::Config;

#[derive(Parser)]
#[command(
    name = "kalamar-service",
    version,
    about = "Kalamar community service (reference implementation of protocol 1.1)"
)]
struct Cli {
    /// Configuration file (TOML); see deploy/kalamar.example.toml.
    #[arg(long, short, env = "KALAMAR_CONFIG", default_value = "kalamar.toml")]
    config: PathBuf,
    /// Log format: `text` or `json`.
    #[arg(long, env = "KALAMAR_LOG_FORMAT", default_value = "text")]
    log_format: String,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the service (plain HTTP on `server.bind`; put a TLS reverse proxy in front).
    Serve,
    /// Check the configuration and exit.
    CheckConfig,
    /// Write a starting configuration from a preset (`init --list` names them), ready to adapt.
    Init(kalamar_service::presets::InitArgs),
    /// Create or migrate the database and exit.
    Migrate,
    /// Operator commands.
    #[command(subcommand)]
    Admin(AdminCmd),
}

fn init_logs(format: &str) {
    let filter = tracing_subscriber::EnvFilter::try_from_env("KALAMAR_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let b = tracing_subscriber::fmt().with_env_filter(filter).with_target(false);
    if format == "json" {
        b.json().init();
    } else {
        use std::io::IsTerminal;
        b.with_ansi(std::io::stdout().is_terminal()).init();
    }
}

fn main() -> ExitCode {
    kalamar_service::auth::oauth::install_crypto();
    let cli = Cli::parse();
    init_logs(&cli.log_format);
    if let Cmd::Init(args) = &cli.cmd {
        return match kalamar_service::presets::run(args, &cli.config) {
            Ok(text) => {
                println!("{text}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("{e}");
                ExitCode::from(2)
            }
        };
    }
    let cfg = match Config::load(&cli.config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("configuration error:\n{e}");
            return ExitCode::from(2);
        }
    };
    match cli.cmd {
        Cmd::Init(_) => ExitCode::SUCCESS,
        Cmd::CheckConfig => {
            let missing = cfg.check_secrets();
            if !missing.is_empty() {
                eprintln!("configuration error:\n{}", missing.join("\n"));
                return ExitCode::from(2);
            }
            for w in cfg.warnings() {
                eprintln!("warning: {w}");
            }
            for (a, b) in kalamar_service::relays::shared_secrets_on_disk(&cfg) {
                eprintln!(
                    "warning: relays.{a} and relays.{b} hold the same secret: whoever runs either relay can open rooms on the other (give each relay its own: kalamar-relay --new-room-secret)"
                );
            }
            for d in &cfg.derived {
                println!("note: {d}");
            }
            let logins: Vec<String> = kalamar_service::auth::oauth::configured(&cfg)
                .into_iter()
                .flatten()
                .map(|r| r.id)
                .chain(cfg.auth.password.as_ref().filter(|p| p.enabled).map(|p| {
                    if p.smtp.is_some() {
                        "password (e-mail codes)".to_string()
                    } else {
                        "password (operator reset codes)".to_string()
                    }
                }))
                .chain(cfg.auth.guest.enabled.then(|| "guest".to_string()))
                .chain(cfg.auth.dev.enabled.then(|| "dev".to_string()))
                .collect();
            println!(
                "{}: OK ({} queues, features: {}; logins: {})",
                cli.config.display(),
                cfg.queues.len(),
                cfg.features.declared().join(", "),
                logins.join(", ")
            );
            ExitCode::SUCCESS
        }
        Cmd::Migrate => match kalamar_service::db::open(&cfg.server.database) {
            Ok(_) => {
                println!(
                    "database {} is at version {}",
                    cfg.server.database.display(),
                    kalamar_service::db::MIGRATIONS.len()
                );
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("{e}");
                ExitCode::from(1)
            }
        },
        Cmd::Admin(cmd) => match admin::run(cfg, cmd) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e}");
                ExitCode::from(1)
            }
        },
        Cmd::Serve => {
            let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("runtime: {e}");
                    return ExitCode::from(1);
                }
            };
            rt.block_on(serve(cfg, cli.config))
        }
    }
}

async fn serve(cfg: Config, path: PathBuf) -> ExitCode {
    // Every secret the file names is read before anything starts, with check-config's messages: a missing or
    // unreadable one stops the start instead of failing each match later.
    let missing = cfg.check_secrets();
    if !missing.is_empty() {
        eprintln!("configuration error:\n{}", missing.join("\n"));
        return ExitCode::from(2);
    }
    let clock: Arc<dyn Clock> = if cfg.features.test_clock && !cfg.server.production {
        Arc::new(ManualClock::starting_now())
    } else {
        Arc::new(SystemClock)
    };
    let addr = match cfg.bind_addr() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let st = match kalamar_service::app::build_state(cfg, clock) {
        Ok(mut s) => {
            s.config_path = Some(Arc::new(path));
            s
        }
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    // The relay of [embedded_relay], on a thread of its own, with the very secret the service seals its codes with.
    let secret = st.lock().relay_keys.embedded.clone();
    let embedded = match kalamar_service::relays::start_embedded(&st.cfg(), secret) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{addr}: {e}");
            return ExitCode::from(1);
        }
    };
    // SIGHUP reloads the configuration, like `admin reload` (which also works on Windows).
    #[cfg(unix)]
    {
        let st = st.clone();
        tokio::spawn(async move {
            let Ok(mut hup) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()) else {
                return;
            };
            while hup.recv().await.is_some() {
                kalamar_service::reload::from_file_logged(&st);
            }
        });
    }
    // Ctrl-C, and SIGTERM where there is one (systemd, docker stop): a clean stop, the embedded relay included.
    let shutdown = kalamar_relay::server::shutdown_signal();
    let mut running = match kalamar_service::app::start(st, listener, shutdown).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    tracing::info!(addr = %running.addr, api = %running.state.cfg().server.api_url, "kalamar-service listening");
    let _ = (&mut running.server).await;
    running.ticker.abort();
    running.probe.abort();
    if let Some(relay) = embedded {
        // Every member of the embedded relay gets CLOSED (shutdown), as with kalamar-relay.
        if tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio::task::spawn_blocking(move || relay.stop()),
        )
        .await
        .is_err()
        {
            tracing::warn!("the embedded relay did not stop within 5 s");
        }
    }
    tracing::info!("stopped");
    ExitCode::SUCCESS
}
