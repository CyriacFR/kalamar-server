use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use kalamar_testkit::client::{Api, Events, WsAuth};
use kalamar_testkit::conformance::{self, Options};
use kalamar_testkit::sim::{Play, Sim};
use serde_json::json;

#[derive(Parser)]
#[command(
    name = "kalamar-testkit",
    version,
    about = "Simulated launchers and the conformance suite of the Kalamar community protocol"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the conformance scenarios against a server (a test instance: it creates accounts and matches).
    Conformance {
        /// The API base URL (`api` of the server card), e.g. https://test.example.org/api.
        #[arg(long)]
        api: String,
        /// Where the discovery document is (`host[:port]` or a URL); default: the API's origin.
        #[arg(long)]
        discovery: Option<String>,
        /// Comma-separated areas to run (default: all; undeclared areas are reported as such).
        #[arg(long, value_delimiter = ',')]
        areas: Option<Vec<String>>,
        /// The direct login provider to create accounts with (default: `dev`, else the first usable one).
        #[arg(long)]
        provider: Option<String>,
        /// A file of access tokens of test accounts, one per line (servers without a direct provider).
        #[arg(long)]
        tokens: Option<PathBuf>,
        /// Longest real wait accepted for a deadline when the server has no test clock (seconds).
        #[arg(long, default_value_t = 30)]
        max_wait: u64,
        /// Write the machine-readable report here.
        #[arg(long)]
        json: Option<PathBuf>,
        #[arg(long)]
        verbose: bool,
        /// 1.1.9 `core.reload`: a shell command that makes the server read its configuration again (for the
        /// reference server: `kalamar-service -c <file> admin reload`). Without it the scenario is skipped.
        #[arg(long)]
        reload_command: Option<String>,
    },
    /// Play one match through a private room with simulated launchers and print the decision.
    Simulate {
        #[arg(long)]
        api: String,
        /// Number of players (2 to 8).
        #[arg(long, default_value_t = 2)]
        players: usize,
        /// The direct provider to log the simulated players in with.
        #[arg(long, default_value = "dev")]
        provider: String,
        /// Seat (0-based) that uploads a cheated result.
        #[arg(long)]
        cheat: Option<usize>,
        /// Seat that stays silent after reaching the match.
        #[arg(long)]
        silent: Option<usize>,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    kalamar_testkit::client::install_crypto();
    match Cli::parse().cmd {
        Cmd::Conformance {
            api,
            discovery,
            areas,
            provider,
            tokens,
            max_wait,
            json: json_out,
            verbose,
            reload_command,
        } => {
            let tokens = match tokens {
                Some(p) => match std::fs::read_to_string(&p) {
                    Ok(t) => t
                        .lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .map(str::to_string)
                        .collect(),
                    Err(e) => {
                        eprintln!("{}: {e}", p.display());
                        return ExitCode::from(2);
                    }
                },
                None => Vec::new(),
            };
            let report = conformance::run(Options {
                api,
                discovery,
                areas,
                provider,
                tokens,
                max_wait,
                verbose,
                reload_command,
            })
            .await;
            print!("{}", report.table());
            if let Some(p) = json_out
                && let Err(e) = std::fs::write(&p, serde_json::to_string_pretty(&report.to_json()).unwrap_or_default())
            {
                eprintln!("{}: {e}", p.display());
            }
            if report.passed() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Cmd::Simulate {
            api,
            players,
            provider,
            cheat,
            silent,
        } => match simulate(&api, players, &provider, cheat, silent).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e}");
                ExitCode::from(1)
            }
        },
    }
}

async fn simulate(
    base: &str,
    n: usize,
    provider: &str,
    cheat: Option<usize>,
    silent: Option<usize>,
) -> Result<(), String> {
    let anon = Api::new(base);
    let mut apis = Vec::new();
    let mut events = Vec::new();
    for i in 0..n.clamp(2, 8) {
        let field = if provider == "guest" {
            "display_name"
        } else {
            "username"
        };
        let r = anon
            .post(
                &format!("/v1/auth/{provider}/login"),
                json!({ field: format!("sim{i}") }),
            )
            .await;
        let token = r.body["access_token"]
            .as_str()
            .ok_or(format!("login: {} {}", r.status, r.body))?
            .to_string();
        events.push(Events::connect(base, WsAuth::Bearer(&token), None).await?);
        apis.push(anon.with_token(&token));
    }
    // Leftovers of an earlier run (dev logins map to the same accounts).
    for a in &apis {
        if let Some(id) = a.get("/v1/rooms/current").await.body["room"]["id"].as_str() {
            a.post_empty(&format!("/v1/rooms/{id}/leave")).await;
        }
    }
    let r = apis[0].post("/v1/rooms", json!({"options": {}})).await;
    let room = r.body["id"].as_str().ok_or(format!("room: {}", r.body))?.to_string();
    let code = r.body["code"].as_str().unwrap_or_default().to_string();
    for a in &apis[1..] {
        a.post("/v1/rooms/join", json!({"code": code})).await;
        a.post(&format!("/v1/rooms/{room}/ready"), json!({"ready": true})).await;
    }
    let r = apis[0].post_empty(&format!("/v1/rooms/{room}/start")).await;
    let mid = r.body["id"].as_str().ok_or(format!("start: {}", r.body))?.to_string();
    println!("match {mid} ready: {}", r.body["game"]["rules"]);
    let mut views = Vec::new();
    for a in &apis {
        views.push(a.get(&format!("/v1/matches/{mid}")).await.body);
    }
    let sim = Sim::from_match(&views[0]).ok_or("no game block")?;
    let plays: Vec<Play> = (0..sim.seats.len())
        .map(|s| {
            if Some(s) == cheat {
                Play::Cheat
            } else if Some(s) == silent {
                Play::Silent { played: true }
            } else {
                Play::Honest
            }
        })
        .collect();
    for (k, a) in apis.iter().enumerate() {
        let seat = sim
            .seat_of(views[k]["you"]["member_name"].as_str().unwrap_or_default())
            .unwrap_or(0);
        a.post(&format!("/v1/matches/{mid}/launch"), json!({})).await;
        for st in sim.statuses(seat, &plays[seat]) {
            a.post(&format!("/v1/matches/{mid}/status"), st).await;
        }
        if let Some(res) = sim.result_for(seat, &plays) {
            a.post(&format!("/v1/matches/{mid}/result"), res).await;
        }
    }
    println!("waiting for the decision (a silent player waits for the policy's grace period)...");
    let ev = &mut events[0];
    let done = ev
        .wait_any(
            &["match.completed", "match.void"],
            |e| e["data"]["id"] == mid.as_str(),
            Duration::from_secs(600),
        )
        .await
        .ok_or("no decision")?;
    println!("state: {}", done["data"]["state"]);
    println!(
        "{}",
        serde_json::to_string_pretty(&done["data"]["outcome"]).unwrap_or_default()
    );
    Ok(())
}
