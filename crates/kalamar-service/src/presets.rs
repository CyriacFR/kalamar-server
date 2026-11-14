//! Starting configurations (`kalamar-service init --preset <name>`): complete, commented files for the usual kinds of
//! community, so that an operator adapts a short file instead of the full reference (`deploy/kalamar.example.toml`).
//! Every preset is a valid configuration once rendered (tested), and only uses what the reference server offers.

use std::path::Path;

use clap::Args;

use crate::config::Config;

pub struct Preset {
    pub id: &'static str,
    pub summary: &'static str,
    pub text: &'static str,
}

pub const PRESETS: &[Preset] = &[
    Preset {
        id: "friends",
        summary: "a small private server: rooms by code, friends list, a name as the only login",
        text: include_str!("../presets/friends.toml"),
    },
    Preset {
        id: "competitive",
        summary: "ranked 1 v 1 and 4 v 4 queues, ladders and tiers, stage veto, parties, referees' seats",
        text: include_str!("../presets/competitive.toml"),
    },
    Preset {
        id: "tournament",
        summary: "an 8-player cup and a best-of-5 challenge, stage drafts, public stands, winners' badges",
        text: include_str!("../presets/tournament.toml"),
    },
    Preset {
        id: "full",
        summary: "the reference file with every option and its explanation (values marked CHANGE are left to you)",
        text: include_str!("../../../deploy/kalamar.example.toml"),
    },
];

#[derive(Args, Debug, Clone)]
pub struct InitArgs {
    /// The preset to start from (see --list).
    #[arg(long, default_value = "friends")]
    pub preset: String,
    /// List the presets and exit.
    #[arg(long)]
    pub list: bool,
    /// The server's name, shown in launchers (1 to 64 characters).
    #[arg(long, default_value = "My community")]
    pub name: String,
    /// The public host name players type in their launcher (HTTPS).
    #[arg(long, default_value = "play.example.org")]
    pub host: String,
    /// The relay's public host name (UDP 47313); default: the same as --host.
    #[arg(long)]
    pub relay: Option<String>,
    /// Your name or your team's, shown to players.
    #[arg(long, default_value = "CHANGE")]
    pub operator: String,
    /// How players reach you: a mailto: or https: link.
    #[arg(long, default_value = "mailto:admin@example.org")]
    pub contact: String,
    /// Address the service listens on (plain HTTP): `0.0.0.0:8080` in a container, behind its proxy.
    #[arg(long, default_value = "127.0.0.1:8080")]
    pub bind: String,
    /// Folder of the database and the secret files: `/var/lib/kalamar` with the systemd unit, `/data` in the image.
    #[arg(long, default_value = "data")]
    pub data_dir: String,
    /// The HTTPS proxy's addresses (CIDR), the only peers whose X-Forwarded-For is believed.
    #[arg(long, value_delimiter = ',', default_value = "127.0.0.1/32,::1/128")]
    pub trusted_proxies: Vec<String>,
    /// Replace the file if it exists.
    #[arg(long)]
    pub force: bool,
    /// Do not create the relay secret the configuration names (an installer that creates it under the service's
    /// own account does it itself). By default it is created, readable by its owner only on Unix.
    #[arg(long)]
    pub no_relay_secret: bool,
}

/// A value inside a TOML basic string.
fn quoted(s: &str) -> Result<String, String> {
    if s.chars().any(char::is_control) {
        return Err("values cannot hold control characters".into());
    }
    Ok(s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn host_ok(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 253
        && h.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'))
}

/// The preset's text with the operator's values in place.
pub fn render(a: &InitArgs) -> Result<String, String> {
    let p = PRESETS
        .iter()
        .find(|p| p.id == a.preset)
        .ok_or_else(|| format!("unknown preset `{}`\n{}", a.preset, list()))?;
    let relay = a
        .relay
        .as_deref()
        .unwrap_or_else(|| a.host.split(':').next().unwrap_or(&a.host));
    if !host_ok(&a.host) || !host_ok(relay) || relay.contains(':') {
        return Err("--host and --relay are host names (play.example.org), without https:// or a path".into());
    }
    if a.bind.parse::<std::net::SocketAddr>().is_err() {
        return Err("--bind is an address and a port (127.0.0.1:8080)".into());
    }
    if a.trusted_proxies
        .iter()
        .any(|c| crate::netutil::Cidr::parse(c).is_none())
    {
        return Err("--trusted-proxies are CIDRs (127.0.0.1/32,172.16.0.0/12)".into());
    }
    let proxies: Vec<String> = a.trusted_proxies.iter().map(|c| format!("\"{c}\"")).collect();
    let data = quoted(a.data_dir.trim_end_matches('/'))?;
    if p.id == "full" {
        return render_full(p.text, a, relay, &data, &proxies.join(", "));
    }
    Ok(p.text
        .replace("{{BIND}}", &a.bind)
        .replace("{{DATA}}", &data)
        .replace("{{PROXIES}}", &proxies.join(", "))
        .replace("{{NAME}}", &quoted(&a.name)?)
        .replace("{{HOST}}", &a.host)
        .replace("{{RELAY}}", relay)
        .replace("{{OPERATOR}}", &quoted(&a.operator)?)
        .replace("{{CONTACT}}", &quoted(&a.contact)?))
}

/// The code part of a TOML line and its comment (`#` outside a string).
fn split_comment(line: &str) -> (&str, &str) {
    let mut in_str = false;
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        match c {
            '\\' if in_str && !escaped => {
                escaped = true;
                continue;
            }
            '"' if !escaped => in_str = !in_str,
            '#' if !in_str => return (&line[..i], &line[i..]),
            _ => {}
        }
        escaped = false;
    }
    (line, "")
}

/// The preset `full` is the reference file itself (`deploy/kalamar.example.toml`, no placeholders): the values of
/// `init` replace its examples, line by line, in `[server]` and in the first `[[relays]]`; every path under `data/`
/// moves to `--data-dir`. The comments stay, without their `CHANGE` mark where a value was given.
fn render_full(text: &str, a: &InitArgs, relay: &str, data: &str, proxies: &str) -> Result<String, String> {
    let values: Vec<(&str, String)> = vec![
        ("name", format!("\"{}\"", quoted(&a.name)?)),
        ("api_url", format!("\"https://{}/api\"", a.host)),
        ("bind", format!("\"{}\"", a.bind)),
        ("trusted_proxies", format!("[{proxies}]")),
        ("privacy_url", format!("\"https://{}/privacy\"", a.host)),
        ("rules_url", format!("\"https://{}/rules\"", a.host)),
        (
            "operator",
            format!(
                "{{ name = \"{}\", contact = \"{}\" }}",
                quoted(&a.operator)?,
                quoted(&a.contact)?
            ),
        ),
    ];
    let mut out = String::with_capacity(text.len() + 256);
    let mut section = String::new();
    let mut relays_seen = 0;
    let mut applied = std::collections::BTreeSet::new();
    for line in text.lines() {
        let (code, comment) = split_comment(line);
        let head = code.trim();
        // A table header (`[server]`, `[[relays]]`), not a line of a multi-line array.
        if head.starts_with('[') && head.ends_with(']') && !head.contains(['=', ',', '"']) {
            section = head.trim_matches(['[', ']']).to_string();
            if section == "relays" {
                relays_seen += 1;
            }
        }
        let key = code.split('=').next().unwrap_or("").trim();
        let value = match (section.as_str(), key) {
            ("server", k) if code.contains('=') => values.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone()),
            ("relays", "host") if relays_seen == 1 && code.contains('=') => Some(format!("\"{relay}\"")),
            _ => None,
        };
        let line = match value {
            Some(v) => {
                applied.insert(format!("{section}.{key}"));
                let comment = comment
                    .replacen("# CHANGE: ", "# ", 1)
                    .replacen("# CHANGE or remove", "", 1);
                let comment = if comment.trim() == "# CHANGE" {
                    ""
                } else {
                    comment.trim_end()
                };
                let kv = format!("{key} = {v}");
                if comment.is_empty() {
                    kv
                } else {
                    format!("{kv:<56} {comment}")
                }
            }
            None => line.to_string(),
        };
        out.push_str(&line.replace("\"data/", &format!("\"{data}/")));
        out.push('\n');
    }
    // The reference file must keep these lines: a test fails if one goes missing.
    for k in [
        "server.name",
        "server.api_url",
        "server.bind",
        "server.operator",
        "relays.host",
    ] {
        if !applied.contains(k) {
            return Err(format!("the reference file has no `{k}` line to fill in"));
        }
    }
    Ok(out)
}

pub fn list() -> String {
    let mut s = String::from("presets:\n");
    for p in PRESETS {
        s.push_str(&format!("  {:<12} {}\n", p.id, p.summary));
    }
    s.trim_end().to_string()
}

/// `kalamar-service --config <out> init ...`: writes the file and says what to do next.
pub fn run(a: &InitArgs, out: &Path) -> Result<String, String> {
    if a.list {
        return Ok(list());
    }
    let text = render(a)?;
    // The file must be a configuration this binary accepts before it is written.
    Config::parse(&text).map_err(|e| format!("the preset does not fit these values:\n{e}"))?;
    if out.exists() && !a.force {
        return Err(format!(
            "{} exists: choose another file with --config, or replace it with --force",
            out.display()
        ));
    }
    if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(out, &text).map_err(|e| format!("{}: {e}", out.display()))?;
    let cfg = Config::parse(&text)?;
    // The relay secrets it names, so that check-config passes right away (an existing file is kept).
    let mut created = Vec::new();
    if !a.no_relay_secret {
        let files = cfg.relays.iter().filter_map(|r| r.secret_file.clone()).chain(
            cfg.embedded_relay
                .secret_file
                .clone()
                .filter(|_| cfg.embedded_relay.enabled),
        );
        for f in files {
            if f.exists()
                || created
                    .iter()
                    .any(|c: &std::path::PathBuf| crate::config::same_file(c, &f))
            {
                continue;
            }
            if let Some(dir) = f.parent().filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            kalamar_relay::roomauth::create_secret_file(&f).map_err(|e| format!("{}: {e}", f.display()))?;
            created.push(f);
        }
    }
    Ok(next_steps(a, out, &cfg, &created))
}

/// What to do after `init`, for the configuration just written (and the secrets just created).
fn next_steps(a: &InitArgs, out: &Path, cfg: &Config, created: &[std::path::PathBuf]) -> String {
    let c = out.display();
    let mut steps = vec!["read it: every line is yours to change".to_string()];
    for f in created {
        steps.push(format!(
            "back up {} with the database: the relay's secret, just created (readable by its owner only on Unix)",
            f.display()
        ));
    }
    let relay = &cfg.relays[0];
    let secret = relay.secret_file.as_ref().filter(|_| !cfg.embedded_relay.enabled);
    if let Some(f) = secret.filter(|f| !f.exists()) {
        steps.push(format!(
            "kalamar-relay --new-room-secret {}      (the secret that binds the relay to this server)",
            f.display()
        ));
    }
    steps.push(format!("kalamar-service -c {c} check-config"));
    if cfg.embedded_relay.enabled {
        steps.push(format!(
            "kalamar-service -c {c} serve      (the API behind an HTTPS proxy: deploy/Caddyfile; its relay inside, on UDP {}, which players must reach directly)",
            cfg.embedded_relay.port
        ));
    } else {
        let bind = match secret {
            Some(f) => format!(" --room-secret-file {}", f.display()),
            // A relay without a secret starts only when told it is meant to be open.
            None => " --open".to_string(),
        };
        steps.push(format!(
            "kalamar-relay --port {}{bind}      (the relay: UDP {}, which players must reach directly)",
            relay.port, relay.port
        ));
        steps.push(format!(
            "kalamar-service -c {c} serve      (the API, behind an HTTPS proxy: deploy/Caddyfile)"
        ));
    }
    let mut next = format!("wrote {c} (preset `{}`)\n\nnext:\n", a.preset);
    for (i, s) in steps.iter().enumerate() {
        next.push_str(&format!("  {}. {s}\n", i + 1));
    }
    next.push_str(&format!(
        "  later: edit the file, then  kalamar-service -c {c} admin reload"
    ));
    if a.host == "play.example.org" || a.operator == "CHANGE" || a.preset == "full" {
        next.push_str(
            "\n\nplaceholders may be left in it (example.org, CHANGE): pass --host, --operator and --contact, or edit them",
        );
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(preset: &str) -> InitArgs {
        InitArgs {
            preset: preset.into(),
            list: false,
            name: "The \"Thursday\" \\ club".into(),
            host: "play.example.org".into(),
            relay: None,
            operator: "Camille".into(),
            contact: "mailto:camille@example.org".into(),
            bind: "127.0.0.1:8080".into(),
            data_dir: "data".into(),
            trusted_proxies: vec!["127.0.0.1/32".into(), "::1/128".into()],
            force: false,
            // The tests write their configurations under the temporary folder only.
            no_relay_secret: true,
        }
    }

    #[test]
    fn every_preset_is_a_valid_configuration() {
        for p in PRESETS {
            let text = render(&args(p.id)).unwrap();
            assert!(!text.contains("{{"), "{}: a placeholder is left", p.id);
            let cfg = Config::parse(&text).unwrap_or_else(|e| panic!("{}: {e}", p.id));
            assert!(cfg.server.production, "{}", p.id);
            assert_eq!(cfg.relays.len(), 1, "{}", p.id);
            // Every preset, `full` included, takes the values given to `init`.
            assert_eq!(cfg.server.name, "The \"Thursday\" \\ club", "{}", p.id);
            assert_eq!(cfg.server.api_url, "https://play.example.org/api", "{}", p.id);
            assert_eq!(cfg.relays[0].host, "play.example.org", "{}", p.id);
            assert_eq!(
                (cfg.server.operator.name.as_str(), cfg.server.operator.contact.as_str()),
                ("Camille", "mailto:camille@example.org"),
                "{}",
                p.id
            );
        }
    }

    #[test]
    fn presets_offer_what_they_say() {
        let cfg = |id: &str| Config::parse(&render(&args(id)).unwrap()).unwrap();
        let f = cfg("friends");
        assert!(f.features.rooms && !f.features.queues && !f.features.ratings);
        let c = cfg("competitive");
        assert!(c.features.queues && c.features.ratings && c.features.draft);
        assert!(c.queues.iter().filter(|q| q.rated).count() >= 2);
        let t = cfg("tournament");
        assert!(t.features.activities && t.features.series && !t.features.queues);
        assert_eq!(t.activities.len(), 2);
        // The relay inside the service for a few friends; a relay of its own (bound by a secret) beyond that.
        assert!(f.embedded_relay.enabled && f.embedded_relay.secret_file.is_some());
        assert!(f.embedded_relay.max_sessions <= 256);
        for x in [&c, &t, &cfg("full")] {
            assert!(!x.embedded_relay.enabled);
        }
        for x in [&f, &c, &t] {
            assert!(x.relays[0].secret_file.is_some());
        }
        // Security review M3, M7: ranked play and tournaments without guest accounts, witnesses only from referees;
        // nothing for check-config to warn about. The reference file (`full`) shows the other choices, and is warned.
        for x in [&c, &t] {
            assert!(x.auth.guest.enabled && !x.auth.guest.ranked);
            assert!(x.competitive_warnings().is_empty(), "{:?}", x.competitive_warnings());
            for p in &x.result_policies {
                assert!(
                    p.witness == kalamar_protocol::policy::WitnessUse::Ignore
                        || p.witness_from == kalamar_protocol::policy::WitnessFrom::Referees,
                    "{}",
                    p.id
                );
            }
        }
        let w = cfg("full").competitive_warnings().join("\n");
        assert!(w.contains("auth.guest: guest accounts may play"), "{w}");
    }

    /// `init --preset full` takes the same values as the other presets (none is ignored).
    #[test]
    fn the_full_preset_takes_the_given_values() {
        let mut a = args("full");
        a.host = "games.example.net".into();
        a.relay = Some("relay.example.net".into());
        a.bind = "0.0.0.0:9000".into();
        a.data_dir = "/srv/kalamar/".into();
        a.trusted_proxies = vec!["10.0.0.0/8".into()];
        let text = render(&a).unwrap();
        let c = Config::parse(&text).unwrap();
        assert_eq!(c.server.name, "The \"Thursday\" \\ club");
        assert_eq!(c.server.api_url, "https://games.example.net/api");
        assert_eq!(c.server.privacy_url, "https://games.example.net/privacy");
        assert_eq!(c.server.bind, "0.0.0.0:9000");
        assert_eq!(
            c.server.database,
            std::path::PathBuf::from("/srv/kalamar/kalamar.sqlite3")
        );
        assert_eq!(
            c.server.secret_file,
            std::path::PathBuf::from("/srv/kalamar/secret.key")
        );
        assert_eq!(c.server.trusted_proxies, vec!["10.0.0.0/8"]);
        assert_eq!(c.relays[0].host, "relay.example.net");
        assert!(!text.contains("\"data/"), "every path moved to --data-dir");
        assert!(
            text.contains("# secret_file = \"/srv/kalamar/relay.key\""),
            "commented paths too"
        );
        // The comments stay; the other sections are untouched.
        assert!(text.contains("# Kalamar community service: example configuration"));
        let reference = Config::parse(PRESETS.iter().find(|p| p.id == "full").unwrap().text).unwrap();
        assert_eq!(c.queues.len(), reference.queues.len());
        assert_eq!(
            format!("{:?}", c.result_policies),
            format!("{:?}", reference.result_policies)
        );
    }

    /// The reference file with its [embedded_relay] block uncommented, and nothing else changed, is a working
    /// configuration: the only [[relays]] entry gets the embedded relay's secret.
    #[test]
    fn uncommenting_the_embedded_relay_of_the_reference_file_works() {
        let text = PRESETS.iter().find(|p| p.id == "full").unwrap().text;
        let mut lines: Vec<String> = Vec::new();
        let mut inside = false;
        for l in text.lines() {
            if l.starts_with("# [embedded_relay]") {
                inside = true;
            }
            if inside && !l.starts_with('#') {
                inside = false;
            }
            let setting = l.starts_with("# ")
                && (l[2..].starts_with('[')
                    || l[2..].starts_with(|c: char| c.is_ascii_lowercase()) && l.contains(" = "));
            if inside && setting {
                lines.push(l[2..].to_string());
            } else {
                lines.push(l.to_string());
            }
        }
        let uncommented = lines.join("\n");
        assert!(
            uncommented.contains("\n[embedded_relay]\nenabled = true"),
            "{uncommented}"
        );
        let c = Config::parse(&uncommented).unwrap_or_else(|e| panic!("{e}"));
        assert!(c.embedded_relay.enabled);
        assert_eq!(c.relays[0].secret_file, c.embedded_relay.secret_file);
        assert_eq!(c.derived.len(), 1, "{:?}", c.derived);
    }

    #[test]
    fn bad_values_are_refused() {
        let mut a = args("friends");
        a.host = "https://play.example.org".into();
        assert!(render(&a).is_err());
        let mut a = args("friends");
        a.name = "x\ny".into();
        assert!(render(&a).is_err());
        let mut a = args("nope");
        a.list = false;
        assert!(render(&a).unwrap_err().contains("presets:"));
        // A port in --host is the HTTPS port; the relay takes the bare name.
        let mut a = args("friends");
        a.host = "play.example.org:8443".into();
        let c = Config::parse(&render(&a).unwrap()).unwrap();
        assert_eq!(c.relays[0].host, "play.example.org");
    }

    #[test]
    fn a_container_or_a_service_unit_gets_its_own_paths() {
        let mut a = args("competitive");
        a.bind = "0.0.0.0:8080".into();
        a.data_dir = "/var/lib/kalamar/".into();
        a.trusted_proxies = vec!["172.16.0.0/12".into(), "10.0.0.0/8".into()];
        let c = Config::parse(&render(&a).unwrap()).unwrap();
        assert_eq!(c.server.bind, "0.0.0.0:8080");
        assert_eq!(
            c.server.database,
            std::path::PathBuf::from("/var/lib/kalamar/kalamar.sqlite3")
        );
        assert_eq!(c.server.trusted_proxies, vec!["172.16.0.0/12", "10.0.0.0/8"]);
        let key = Some(std::path::PathBuf::from("/var/lib/kalamar/relay.key"));
        assert_eq!(c.relays[0].secret_file, key);
        let mut f = a.clone();
        f.preset = "friends".into();
        let f = Config::parse(&render(&f).unwrap()).unwrap();
        assert_eq!(
            (f.embedded_relay.secret_file.clone(), f.relays[0].secret_file.clone()),
            (key.clone(), key)
        );
        a.bind = "nonsense".into();
        assert!(render(&a).is_err());
        let mut a = args("friends");
        a.trusted_proxies = vec!["everyone".into()];
        assert!(render(&a).is_err());
    }

    #[test]
    fn init_writes_once() {
        let dir = std::env::temp_dir().join(format!("kalamar-init-{}", crate::util::new_id("t")));
        let out = dir.join("kalamar.toml");
        let data = dir.join("data");
        let key = data.join("relay.key");
        // An installer that creates the secret itself: init names the step and creates nothing.
        let mut a = args("competitive");
        a.data_dir = data.to_string_lossy().into_owned();
        a.no_relay_secret = true;
        let said = run(&a, &out).unwrap();
        assert!(said.contains("wrote"));
        // The path as the file names it (`<data dir>/relay.key`).
        let named = Config::load(&out).unwrap().relays[0].secret_file.clone().unwrap();
        assert!(crate::config::same_file(&named, &key));
        let k = named.display().to_string();
        assert!(said.contains(&format!("kalamar-relay --new-room-secret {k}")), "{said}");
        assert!(!key.exists());
        // By default init creates the secret it names, so that check-config passes at once.
        a.no_relay_secret = false;
        a.force = true;
        let said = run(&a, &out).unwrap();
        assert!(key.exists() && !said.contains("--new-room-secret"), "{said}");
        assert!(
            said.contains(&format!("kalamar-relay --port 47313 --room-secret-file {k}")),
            "{said}"
        );
        let cfg = Config::load(&out).unwrap();
        assert!(cfg.check_secrets().is_empty(), "{:?}", cfg.check_secrets());
        let first = kalamar_relay::roomauth::RoomSecret::from_file(&key).unwrap();
        // The friends' preset: the relay inside, the same secret kept.
        let mut a = args("friends");
        a.data_dir = data.to_string_lossy().into_owned();
        a.force = true;
        let said = run(&a, &out).unwrap();
        assert!(
            said.contains("its relay inside, on UDP 47313") && !said.contains("--new-room-secret"),
            "{said}"
        );
        assert_eq!(
            kalamar_relay::roomauth::RoomSecret::from_file(&key).unwrap(),
            first,
            "kept"
        );
        assert!(run(&args("friends"), &out).unwrap_err().contains("exists"));
        let mut a = args("friends");
        a.force = true;
        assert!(run(&a, &out).is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }
}
