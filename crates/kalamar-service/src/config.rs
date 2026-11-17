//! The service's TOML configuration: what the server offers (areas, queues, pools, rooms, ratings, result policies,
//! penalties) and how it runs. Every option is documented in `deploy/kalamar.example.toml`.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use kalamar_protocol::contract::MAX_PLAYERS;
use kalamar_protocol::policy::{
    Abort, Deadlines, Decider, Disagreement, Disconnect, Leave, LoadoutCheck, NoShow, Policy, Quorum, Silence,
    StatsCheck, WitnessFrom, WitnessUse,
};
use serde::Deserialize;

/// Localized text: language tag to string.
pub type Text = BTreeMap<String, String>;

pub fn en(s: &str) -> Text {
    BTreeMap::from([("en".to_string(), s.to_string())])
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub server: ServerSection,
    #[serde(default)]
    pub features: Features,
    #[serde(default)]
    pub auth: AuthSection,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub rate_limits: RateLimits,
    #[serde(default)]
    pub relays: Vec<RelayEntry>,
    /// Runs a relay inside this program, so that a small server is one program to start.
    #[serde(default)]
    pub embedded_relay: EmbeddedRelay,
    /// Protocol 1.1.13: how the load the relays announce weighs in the choice of a match's relay.
    #[serde(default)]
    pub relay_load: RelayLoadSection,
    /// What [`Config::normalize`] filled in by itself (for example the secret of the `[[relays]]` entry that names
    /// the embedded relay), for `check-config` to say.
    #[serde(skip)]
    pub derived: Vec<String>,
    #[serde(default)]
    pub result_policies: Vec<PolicyEntry>,
    #[serde(default)]
    pub rooms: RoomsSection,
    #[serde(default)]
    pub queues: Vec<QueueEntry>,
    #[serde(default)]
    pub penalties: PenaltiesSection,
    #[serde(default)]
    pub ladders: Vec<LadderEntry>,
    #[serde(default)]
    pub rating: RatingSection,
    #[serde(default)]
    pub catalog: Catalog,
    #[serde(default)]
    pub moderation: ModerationSection,
    #[serde(default)]
    pub rules: RulesSection,
    #[serde(default)]
    pub events: EventsSection,
    #[serde(default)]
    pub retention: Retention,
    /// Protocol 1.1: generic activities (`features.activities`).
    #[serde(default)]
    pub activities: Vec<ActivityEntry>,
    /// Outgoing webhooks (`operators.md`, section 1).
    #[serde(default)]
    pub webhooks: Vec<WebhookEntry>,
    /// Bearer tokens of the operator API (`operators.md`, section 2), stored as SHA-256 hashes.
    #[serde(default)]
    pub operator_tokens: Vec<OperatorToken>,
    /// Badges and titles of this server (`features.badges`).
    #[serde(default)]
    pub badges: Vec<BadgeEntry>,
    #[serde(default)]
    pub titles: Vec<TitleEntry>,
    /// Protocol 1.1.6: stage and mode drafts (pick and ban) that queues, rooms and activities name (`draft.md`).
    #[serde(default)]
    pub drafts: Vec<DraftPreset>,
    /// Protocol 1.1.7: the stands, places for spectators in a match (`spectators.md`): referees, opt-out, the wait.
    #[serde(default)]
    pub spectators: SpectatorsSection,
}

/// `[spectators]` (protocol 1.1.7, `spectators.md`): what holds for every place in the stands of this server. Rooms
/// (`[rooms.spectators]`), queues and activities (`spectators = { ... }`) open the places themselves.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SpectatorsSection {
    /// The operator's referees (account ids): they may take any open place whoever it is reserved for, a player's
    /// refusal to be watched does not apply to them, and a result policy may count only their reports
    /// (`witness_from = "referees"`). The operator API also seats them in a given match.
    pub referees: Vec<String>,
    /// Players may refuse to be watched (the account setting `allow_spectators`); off: everyone can be watched.
    pub opt_out: bool,
    /// Offer places only in the modes the players' engines have verified with spectators (`verified_modes`), not in
    /// every mode they declare.
    pub verified_modes_only: bool,
    /// Seconds the host waits for missing spectators once every player is in (the game's `spectators_wait`), unless
    /// a room, queue or activity sets its own.
    pub wait_s: u32,
    /// One sentence about the stands for players (shown with the places).
    pub summary: Text,
}

impl Default for SpectatorsSection {
    fn default() -> Self {
        SpectatorsSection {
            referees: Vec::new(),
            opt_out: true,
            verified_modes_only: false,
            wait_s: 60,
            summary: Text::new(),
        }
    }
}

/// Places in the stands of a queue's or an activity's matches (`spectators = { seats = 1, who = "referees" }`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stands {
    /// Places per match, 1 or 2 (the game has two).
    pub seats: usize,
    /// Who may take one: `anyone`, `friends` (a friend of a player of the match) or `referees` (the operator's).
    #[serde(default = "default_who")]
    pub who: String,
    /// The host's wait for missing spectators (default: `[spectators] wait_s`).
    #[serde(default)]
    pub wait_s: Option<u32>,
}

fn default_who() -> String {
    "anyone".into()
}

/// `[rooms.spectators]`: what a room owner may open in the stands.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RoomStands {
    /// Most places a room may open (0 to 2); 0 hides the option.
    pub max: usize,
    /// Places of a new room.
    pub default: usize,
    /// Who may sit, as offered to the owner (the first is the default): `anyone` (any member of the room),
    /// `invited` (members who came by an invitation), `friends` (the owner's friends), `referees`.
    pub who: Vec<String>,
    /// The game session's host may sit in the stands (else a player hosts).
    pub host: bool,
    /// The host's wait for missing spectators (default: `[spectators] wait_s`).
    pub wait_s: Option<u32>,
}

impl Default for RoomStands {
    fn default() -> Self {
        RoomStands {
            max: 0,
            default: 0,
            who: vec!["anyone".into(), "invited".into(), "friends".into(), "referees".into()],
            host: false,
            wait_s: None,
        }
    }
}

/// The `who` values of the stands (`invited` in rooms only).
pub const STANDS_WHO: &[&str] = &["anyone", "invited", "friends", "referees"];

/// A stage (and mode) draft between "match found" and the launch (`[[drafts]]`, protocol 1.1.6, `draft.md`). Queues
/// (`draft`), rooms (`rooms.drafts`, a form option) and activities (`draft`, for every game of their series) name it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftPreset {
    pub id: String,
    /// Its name (the room form, the queue details, the draft screen's title).
    pub label: Text,
    /// One sentence explaining it to players (shown with the queue and on the draft screen).
    #[serde(default)]
    pub summary: Text,
    /// The turns in order: `ban:<team>:<n>`, `pick:<team>:<n>`, `random[:<n>]`, `last`, `strike`, each optionally
    /// followed by `@mode` (the default kind is `@stage`). Teams: `first`, `second`, `alpha`, `bravo`, `both` (both
    /// teams at once, hidden from each other), `winner`, `loser` (of the previous game of a series).
    pub turns: Vec<String>,
    /// The turns of a series' first game (default: `turns`), e.g. bans before "the loser picks" from game 2 on.
    #[serde(default)]
    pub first_game_turns: Option<Vec<String>>,
    /// Who is `first`: `random`, `higher_rated`, `lower_rated` (the queue's ladder), `loser`, `winner` (previous game
    /// of a series; `random` for the first game), `alpha`, `bravo`.
    #[serde(default = "default_draft_first")]
    pub first: String,
    /// Who acts for a team: `captain` (the party leader, else the first player of the team), `vote` (every player
    /// votes, the majority wins), `any` (the first player of the team to answer).
    #[serde(default = "default_draft_decider")]
    pub decider: String,
    /// Ties of a vote: `captain` (the captain's vote decides) or `random`.
    #[serde(default = "default_draft_tiebreak")]
    pub tiebreak: String,
    /// Time a team has for one turn.
    #[serde(default = "default_draft_turn_s")]
    pub turn_s: u32,
    /// What the server does for a team that lets its turn run out, or whose launchers cannot draft: `random` (it
    /// draws among what remains) or `skip` (the team's bans are lost; a pick is drawn later).
    #[serde(default = "default_draft_timeout")]
    pub timeout: String,
    /// The stage pool (game identifiers of the players' games). Empty: the origin's pool, else every stage all the
    /// players' games can load.
    #[serde(default)]
    pub stages: Vec<String>,
    /// The mode pool of `@mode` turns. Empty: the origin's modes.
    #[serde(default)]
    pub modes: Vec<String>,
    /// For series: `per_game` (a draft before every game) or `once` (one draft before the first game; its picks, in
    /// order, are the stages of the games).
    #[serde(default = "default_draft_series")]
    pub series: String,
    /// In a series, stages already played are out of the next drafts.
    #[serde(default = "yes")]
    pub exclude_played: bool,
    /// Only launchers that show the draft may play (a queue requirement, a room start check); otherwise the server
    /// plays the turns of teams whose launchers cannot.
    #[serde(default)]
    pub required: bool,
}

fn default_draft_first() -> String {
    "random".into()
}
fn default_draft_decider() -> String {
    "captain".into()
}
fn default_draft_tiebreak() -> String {
    "captain".into()
}
fn default_draft_turn_s() -> u32 {
    20
}
fn default_draft_timeout() -> String {
    "random".into()
}
fn default_draft_series() -> String {
    "per_game".into()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebhookEntry {
    /// `https://` (`http://127.0.0.1` only on a non-production server).
    pub url: String,
    /// The signing secret, or `secret_file`.
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default)]
    pub secret_file: Option<PathBuf>,
    /// Event types; empty: all of them.
    #[serde(default)]
    pub events: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorToken {
    pub name: String,
    /// Hex SHA-256 of the token (`kalamar-service admin operator-token` prints both).
    pub sha256: String,
    /// `read`, `results`, `moderation`, `activities`.
    pub scopes: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BadgeEntry {
    pub id: String,
    pub name: Text,
    #[serde(default)]
    pub description: Text,
    #[serde(default = "default_icon")]
    pub icon: String,
    #[serde(default)]
    pub color: Option<String>,
    /// `matches:<n>`, `wins:<n>`, `series_wins:<n>`, `tournament:<activity id>`, `tier:<ladder>:<tier id>`.
    pub rule: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TitleEntry {
    pub id: String,
    pub text: Text,
    /// The badge that unlocks it (none: everyone).
    #[serde(default)]
    pub badge: Option<String>,
}

/// A reference activity (`activities.md`): `tournament` (single elimination, best-of-N series) or `duel_series`
/// (the first two waiting players play a best-of-N series).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityEntry {
    pub id: String,
    pub kind: String,
    pub title: Text,
    #[serde(default)]
    pub subtitle: Text,
    #[serde(default)]
    pub description: Text,
    #[serde(default = "default_icon")]
    pub icon: String,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub order: i64,
    /// Tournament entrants.
    #[serde(default = "default_size")]
    pub size: usize,
    #[serde(default = "default_best_of")]
    pub best_of: u32,
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default = "default_policy")]
    pub result_policy: String,
    /// A finished tournament reopens after this long.
    #[serde(default = "default_reopen")]
    pub reopen_after_s: i64,
    /// Keep players' games open between the games of a series (match contract 2 rounds, same host and relay
    /// room). Used only when every player's engine declares rounds faster than a restart.
    #[serde(default)]
    pub keep_open: bool,
    /// Weapon and gear constraints of every match (`match-contract-2.md`, section 5): the JSON of `rules.loadout`.
    #[serde(default)]
    pub loadout: Option<serde_json::Value>,
    /// 1.1.6: the stage draft of the games of this activity's series (a `[[drafts]]` id).
    #[serde(default)]
    pub draft: Option<String>,
    /// 1.1.7: places in the stands of the activity's series (a place holds for the next games of the series).
    #[serde(default)]
    pub spectators: Option<Stands>,
}

/// A named loadout a room owner can pick in the room form.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoadoutPreset {
    pub id: String,
    pub label: Text,
    /// The JSON of `rules.loadout`.
    pub loadout: serde_json::Value,
}

fn default_icon() -> String {
    "trophy".into()
}
fn default_size() -> usize {
    4
}
fn default_best_of() -> u32 {
    3
}
fn default_mode() -> String {
    "turf".into()
}
fn default_policy() -> String {
    "standard".into()
}
fn default_reopen() -> i64 {
    600
}

impl ActivityEntry {
    pub fn kind_label(&self) -> Text {
        match self.kind.as_str() {
            "tournament" => en("Tournament"),
            "duel_series" => en("Series challenge"),
            other => en(other),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSection {
    pub name: String,
    #[serde(default)]
    pub description: Text,
    /// The API base URL announced to launchers (`https://host[:port][/path]`). Its path is where the API is served.
    pub api_url: String,
    /// Address the HTTP server listens on (plain HTTP; a reverse proxy terminates TLS).
    #[serde(default = "default_bind")]
    pub bind: String,
    /// A production server refuses the `dev` provider and the test clock.
    #[serde(default = "yes")]
    pub production: bool,
    #[serde(default = "default_database")]
    pub database: PathBuf,
    /// 32 random bytes (hex) keying the identity hashes; created on first start.
    #[serde(default = "default_secret_file")]
    pub secret_file: PathBuf,
    /// Peers whose `X-Forwarded-For` is trusted (CIDR, IPv4 or IPv6). Empty: the socket peer is the client.
    #[serde(default)]
    pub trusted_proxies: Vec<String>,
    pub operator: Operator,
    pub privacy_url: String,
    #[serde(default)]
    pub rules_url: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub languages: Vec<String>,
    /// With several `[[relays]]`, how a match gets one: `latency` (the relay whose slowest player is the fastest,
    /// from what launchers measured; the least busy one when nobody measured), `least_busy`, or `first`.
    #[serde(default = "default_relay_selection")]
    pub relay_selection: String,
    /// With several `[[relays]]`, seconds between two reachability checks of each (a relay that fails three in a row
    /// gets no new match while another answers); 0: never checked.
    #[serde(default = "default_relay_probe")]
    pub relay_probe_s: u64,
}

fn default_bind() -> String {
    "127.0.0.1:8080".into()
}
fn default_database() -> PathBuf {
    PathBuf::from("data/kalamar.sqlite3")
}
fn default_secret_file() -> PathBuf {
    PathBuf::from("data/secret.key")
}
fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operator {
    pub name: String,
    pub contact: String,
}

/// Optional areas. `presence` needs `friends`, `leaderboards` needs `ratings`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Features {
    pub rooms: bool,
    pub queues: bool,
    pub parties: bool,
    pub friends: bool,
    pub presence: bool,
    pub ratings: bool,
    pub leaderboards: bool,
    pub stats: bool,
    pub moderation: bool,
    /// `POST /v1/_test/clock`; refused in production.
    pub test_clock: bool,
    /// Protocol 1.1 areas (off unless configured).
    pub activities: bool,
    pub series: bool,
    pub badges: bool,
    pub profiles: bool,
    /// Protocol 1.1.4: `GET /v1/population` (how many players are online, in a match, searching).
    pub population: bool,
    /// Protocol 1.1.6: stage and mode drafts between "match found" and the launch (`[[drafts]]`).
    pub draft: bool,
    /// Protocol 1.1.7: places for spectators in matches (`[spectators]`, `spectators.md`).
    pub spectators: bool,
}

impl Default for Features {
    fn default() -> Self {
        Features {
            rooms: true,
            queues: true,
            parties: true,
            friends: true,
            presence: true,
            ratings: true,
            leaderboards: true,
            stats: true,
            moderation: true,
            test_clock: false,
            activities: false,
            series: false,
            badges: false,
            profiles: false,
            population: false,
            draft: false,
            spectators: false,
        }
    }
}

impl Features {
    /// The `features` list of the discovery document and `GET /v1/meta`.
    pub fn declared(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        for (on, id) in [
            (self.rooms, "rooms"),
            (self.queues, "queues"),
            (self.parties, "parties"),
            (self.friends, "friends"),
            (self.presence, "presence"),
            (self.ratings, "ratings"),
            (self.leaderboards, "leaderboards"),
            (self.stats, "stats"),
            (self.moderation, "moderation"),
            (self.activities, "activities"),
            (self.series, "series"),
            (self.badges, "badges"),
            (self.profiles, "profiles"),
            (self.population, "population"),
            (self.draft, "draft"),
            (self.spectators, "spectators"),
            (self.test_clock, "test-clock"),
        ] {
            if on {
                v.push(id);
            }
        }
        v
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuthSection {
    pub dev: Toggle,
    pub guest: GuestConfig,
    pub discord: Option<OAuthConfig>,
    /// Any OAuth 2 / OpenID Connect provider (`[[auth.oauth]]`), configured without code.
    pub oauth: Vec<OAuthEntry>,
    /// E-mail and password accounts (`[auth.password]`).
    pub password: Option<PasswordConfig>,
}

/// A generic OAuth 2 authorization-code provider (`[[auth.oauth]]`). A `preset` fills the endpoints and the user-id
/// path of a known provider; every value can still be given explicitly. See `deploy/kalamar.example.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OAuthEntry {
    #[serde(default = "yes")]
    pub enabled: bool,
    /// The provider's id in the API paths and in the stored identity hash: never change it once players use it.
    pub id: String,
    /// `discord`, `google`, `github`, `twitch` or `oidc` (endpoints from `issuer`).
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default)]
    pub label: Text,
    #[serde(default)]
    pub client_id: String,
    /// A file holding the client secret (only the service should read it).
    #[serde(default)]
    pub client_secret_file: Option<PathBuf>,
    /// Or the name of an environment variable holding it.
    #[serde(default)]
    pub client_secret_env: Option<String>,
    /// Loopback redirect URIs registered with the provider, in order of preference.
    #[serde(default)]
    pub redirect_uris: Vec<String>,
    /// OpenID Connect discovery: `<issuer>/.well-known/openid-configuration` gives the endpoints not set below.
    #[serde(default)]
    pub issuer: Option<String>,
    #[serde(default)]
    pub authorize_url: Option<String>,
    #[serde(default)]
    pub token_url: Option<String>,
    #[serde(default)]
    pub userinfo_url: Option<String>,
    #[serde(default)]
    pub scopes: Option<Vec<String>>,
    /// Send the launcher's PKCE challenge and verifier to the provider too (the server checks them anyway).
    #[serde(default)]
    pub upstream_pkce: Option<bool>,
    /// Path of the provider's stable user id in the userinfo JSON (`sub`, `id`, `data.0.id`).
    #[serde(default)]
    pub subject: Option<String>,
    /// Paths of the initial display name, first found wins (`[]`: "Player").
    #[serde(default)]
    pub name: Option<Vec<String>>,
    /// Client authentication at the token endpoint: `basic` (HTTP Basic) or `post` (in the form).
    #[serde(default)]
    pub token_auth: Option<String>,
    /// A header carrying the client id on the userinfo request (Twitch: `Client-Id`).
    #[serde(default)]
    pub userinfo_client_id_header: Option<String>,
    /// Extra parameters of the authorization URL (Discord: `prompt = "consent"`).
    #[serde(default)]
    pub authorize_params: Option<BTreeMap<String, String>>,
}

/// E-mail and password accounts (`[auth.password]`): direct forms the launcher draws from their fields.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PasswordConfig {
    pub enabled: bool,
    /// Shortest password accepted for a new password (Unicode characters).
    pub min_length: usize,
    /// Longest password accepted (bounds the hashing work).
    pub max_length: usize,
    /// The "Create an account" form (false: no new password accounts).
    pub signup: bool,
    /// The "Change password" form of the login screen (address, current and new password).
    pub change_form: bool,
    /// Validity of a code sent by e-mail.
    pub email_code_ttl_s: i64,
    /// Validity of a reset code made by `admin password-reset`.
    pub operator_code_ttl_s: i64,
    /// Codes e-mailed to one address per hour (sign-up and reset together).
    pub codes_per_hour: f64,
    pub argon2: Argon2Config,
    /// Password hashes computed at once (each holds `argon2.memory_kib` of memory).
    pub max_concurrent_hashes: usize,
    pub lockout: LockoutConfig,
    /// Outgoing e-mail for codes; without it, resets use codes made by the operator.
    pub smtp: Option<SmtpConfig>,
}

impl Default for PasswordConfig {
    fn default() -> Self {
        PasswordConfig {
            enabled: false,
            min_length: 10,
            max_length: 128,
            signup: true,
            change_form: true,
            email_code_ttl_s: 900,
            operator_code_ttl_s: 3600,
            codes_per_hour: 3.0,
            argon2: Argon2Config::default(),
            max_concurrent_hashes: 2,
            lockout: LockoutConfig::default(),
            smtp: None,
        }
    }
}

/// Argon2id cost (OWASP Password Storage Cheat Sheet: 19 MiB, 2 iterations, 1 lane, or an equivalent trade-off).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Argon2Config {
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
}

impl Default for Argon2Config {
    fn default() -> Self {
        Argon2Config {
            memory_kib: 19_456,
            iterations: 2,
            parallelism: 1,
        }
    }
}

/// Failed password checks: token buckets (in memory) per account and per address. Only failures spend tokens.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LockoutConfig {
    /// Failures allowed at once on one account (from every address but those it logged in from), then one more per
    /// `account_refill_s`.
    pub account_failures: f64,
    pub account_refill_s: f64,
    /// Failures allowed at once from one address (every account), then one more per `ip_refill_s`.
    pub ip_failures: f64,
    pub ip_refill_s: f64,
    /// An address an account logged in from stays exempt from that account's lock this long (memory only).
    pub known_address_days: i64,
}

impl Default for LockoutConfig {
    fn default() -> Self {
        LockoutConfig {
            account_failures: 10.0,
            account_refill_s: 120.0,
            ip_failures: 30.0,
            ip_refill_s: 60.0,
            known_address_days: 30,
        }
    }
}

/// Outgoing e-mail (`[auth.password.smtp]`): TLS always (`implicit` or `starttls`); `none` only towards a test relay
/// on 127.0.0.1 of a non-production server.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SmtpConfig {
    pub host: String,
    #[serde(default)]
    pub port: Option<u16>,
    /// `starttls` (port 587 by default), `implicit` (465) or `none` (tests on loopback only).
    #[serde(default = "default_smtp_tls")]
    pub tls: String,
    #[serde(default)]
    pub username: Option<String>,
    /// A file holding the SMTP password, or `password_env`: never in this file.
    #[serde(default)]
    pub password_file: Option<PathBuf>,
    #[serde(default)]
    pub password_env: Option<String>,
    /// The sender, `Name <address>` or `address`.
    pub from: String,
    /// E-mail a code to confirm the address before an account is created.
    #[serde(default = "yes")]
    pub verify_signup: bool,
    #[serde(default = "default_smtp_timeout")]
    pub timeout_s: u64,
}

fn default_smtp_tls() -> String {
    "starttls".into()
}
fn default_smtp_timeout() -> u64 {
    15
}

impl SmtpConfig {
    pub fn port(&self) -> u16 {
        self.port.unwrap_or(match self.tls.as_str() {
            "implicit" => 465,
            "none" => 25,
            _ => 587,
        })
    }
}

/// Reads a secret from a file (trimmed) or an environment variable; `what` names it in errors (never the value).
pub fn read_secret(file: Option<&Path>, env: Option<&str>, what: &str) -> Result<String, String> {
    let v = match (file, env) {
        (Some(f), _) => std::fs::read_to_string(f)
            .map_err(|e| format!("{what}: {}: {e}", f.display()))?
            .trim()
            .to_string(),
        (None, Some(name)) => std::env::var(name).map_err(|_| format!("{what}: the variable {name} is not set"))?,
        (None, None) => return Err(format!("{what}: set a secret file or an environment variable")),
    };
    if v.is_empty() {
        return Err(format!("{what}: the secret is empty"));
    }
    Ok(v)
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Toggle {
    pub enabled: bool,
}

/// `[auth.guest]`: an account with only a display name. `ranked = false` keeps guest accounts out of rated queues and
/// tournaments: a guest account costs one request, so a banned or cooled-down player is back at once, and anyone can
/// bring a second account (security review M7). Default `true`; `check-config` warns.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GuestConfig {
    pub enabled: bool,
    pub ranked: bool,
}

impl Default for GuestConfig {
    fn default() -> Self {
        GuestConfig {
            enabled: false,
            ranked: true,
        }
    }
}

/// An OAuth2 authorization-code provider (Discord by default; the endpoints are configurable for tests).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OAuthConfig {
    pub enabled: bool,
    pub id: String,
    pub label: Text,
    pub client_id: String,
    /// The secret itself (prefer `client_secret_file` or `KALAMAR_DISCORD_CLIENT_SECRET`).
    pub client_secret: Option<String>,
    pub client_secret_file: Option<PathBuf>,
    /// Loopback redirect URIs registered with the provider, in order of preference.
    pub redirect_uris: Vec<String>,
    pub authorize_url: String,
    pub token_url: String,
    pub user_url: String,
    pub scopes: Vec<String>,
    /// Send the launcher's PKCE challenge and verifier to the provider too.
    pub upstream_pkce: bool,
    /// Start a new account's display name from the provider's public name.
    pub use_provider_name: bool,
}

impl Default for OAuthConfig {
    fn default() -> Self {
        OAuthConfig {
            enabled: false,
            id: "discord".into(),
            label: en("Log in with Discord"),
            client_id: String::new(),
            client_secret: None,
            client_secret_file: None,
            redirect_uris: vec!["http://127.0.0.1:53682/callback".into()],
            authorize_url: "https://discord.com/oauth2/authorize".into(),
            token_url: "https://discord.com/api/oauth2/token".into(),
            user_url: "https://discord.com/api/users/@me".into(),
            scopes: vec!["identify".into()],
            upstream_pkce: true,
            use_provider_name: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    pub display_name_min: usize,
    pub display_name_max: usize,
    pub name_change_cooldown_s: i64,
    pub friends_max: usize,
    pub party_size: usize,
    pub body_bytes: usize,
    pub result_bytes: usize,
    pub status_bytes: usize,
    pub friend_request_ttl_s: i64,
    pub party_invite_ttl_s: i64,
    pub room_invite_ttl_s: i64,
    pub access_ttl_s: i64,
    pub refresh_ttl_s: i64,
    pub max_rooms: usize,
    pub max_users: usize,
    /// Security review L-S2: seconds a client has to send a request's headers (slowloris without a proxy in front).
    pub header_timeout_s: u64,
    /// Security review L-S2: open HTTP connections at most (WebSocket event streams excluded); more are refused.
    pub max_connections: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            display_name_min: 1,
            display_name_max: 10,
            name_change_cooldown_s: 3600,
            friends_max: 200,
            party_size: 4,
            body_bytes: 16 * 1024,
            result_bytes: 64 * 1024,
            status_bytes: 4 * 1024,
            friend_request_ttl_s: 30 * 86400,
            party_invite_ttl_s: 300,
            room_invite_ttl_s: 600,
            access_ttl_s: 3600,
            refresh_ttl_s: 30 * 86400,
            max_rooms: 1000,
            max_users: 100_000,
            header_timeout_s: 10,
            max_connections: 4096,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RateLimits {
    pub per_ip_rps: f64,
    pub per_ip_burst: f64,
    pub per_user_rps: f64,
    pub per_user_burst: f64,
    pub auth_per_ip_per_min: f64,
    pub room_join_per_min: f64,
    pub friend_requests_per_hour: f64,
    pub reports_per_day: f64,
    /// `GET /v1/population` per address (a LAN party shares one address: room for several launchers).
    pub population_per_ip_per_min: f64,
    /// 1.1.6: draft actions (bans, picks, votes) per player.
    pub draft_actions_per_min: f64,
    /// 1.1.7: places taken or left in the stands, and lists of watchable matches, per player.
    pub spectate_per_min: f64,
    /// Security review M5: new accounts per client address and day (0: unlimited). A guest login is a new account.
    pub accounts_per_ip_per_day: f64,
    /// Security review M4: room matches started per player and hour (each mints a room code of the relay).
    pub room_starts_per_hour: f64,
    /// Security review M5: the client "address" every per-address limit counts is its network of this prefix length:
    /// IPv4 /32 (one address; 24 for a whole /24) and IPv6 /64 (what one home or one server gets: counting single
    /// IPv6 addresses gave one client 2^64 of them).
    pub ipv4_prefix: u8,
    pub ipv6_prefix: u8,
}

impl Default for RateLimits {
    fn default() -> Self {
        RateLimits {
            per_ip_rps: 30.0,
            per_ip_burst: 120.0,
            per_user_rps: 15.0,
            per_user_burst: 60.0,
            auth_per_ip_per_min: 30.0,
            room_join_per_min: 10.0,
            friend_requests_per_hour: 30.0,
            reports_per_day: 10.0,
            population_per_ip_per_min: 120.0,
            draft_actions_per_min: 60.0,
            spectate_per_min: 20.0,
            accounts_per_ip_per_day: 20.0,
            room_starts_per_hour: 30.0,
            ipv4_prefix: 32,
            ipv6_prefix: 64,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayEntry {
    pub host: String,
    #[serde(default = "default_relay_port")]
    pub port: u16,
    /// A stable name for this relay (`eu`, `us-east`); default: `host:port`. Launchers report their latency under it.
    #[serde(default)]
    pub id: Option<String>,
    /// Where it runs, free text shown to players (`eu-west`).
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub label: Text,
    /// Binds the relay to this service: the secret shared with it (the file given to `kalamar-relay
    /// --room-secret-file`; hexadecimal, at least 32 digits). The service then seals every room code of that relay
    /// with it, and the relay opens no other room. Without it the relay is an open one.
    #[serde(default)]
    pub secret_file: Option<PathBuf>,
    /// The same secret from an environment variable instead of a file.
    #[serde(default)]
    pub secret_env: Option<String>,
}

/// `[embedded_relay]`: the relay run by the service itself (the same code as `kalamar-relay`, on a thread of its own).
/// It is named in `[[relays]]` under the host players reach it by. With a `secret_file` (created at the first start)
/// it opens only the room codes this service seals: the `[[relays]]` entry that names it gets the same secret (taken
/// from here when that entry names none; see [`Config::names_embedded`]).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EmbeddedRelay {
    pub enabled: bool,
    /// Address to listen on (UDP): `0.0.0.0` for every interface.
    pub bind: String,
    pub port: u16,
    pub secret_file: Option<PathBuf>,
    /// Threads that handle the relay's datagrams (`kalamar-relay --workers`): 0, the default, is half the processors
    /// this program may use (at least one: the other half stays for the API); 1 is one thread.
    pub workers: usize,
    /// Most rooms, and most sessions (members of all rooms together), at the same time; 0: no cap (the per-source
    /// limits and the HELLO and JOIN rates still bound what one address holds).
    pub max_rooms: usize,
    pub max_sessions: usize,
    /// What one source (an IPv4 address, an IPv6 /64; loopback is never limited) may hold: sessions of all rooms
    /// together, and members of one room (`kalamar-relay --max-sessions-per-source`, `--max-members-per-source`; 0:
    /// unlimited). The default 8 members of a room is a whole 4 v 4 behind one box; a LAN party with spectators from
    /// one address needs more.
    pub max_sessions_per_source: usize,
    pub max_members_per_source: usize,
    /// Seconds between the relay's one-line statistics in the log (0: none).
    pub stats_interval_s: u64,
}

impl Default for EmbeddedRelay {
    fn default() -> Self {
        let r = kalamar_relay::relay::RelayConfig::default();
        EmbeddedRelay {
            enabled: false,
            bind: "0.0.0.0".into(),
            port: default_relay_port(),
            secret_file: None,
            workers: 0,
            max_rooms: r.max_rooms,
            max_sessions: r.max_sessions,
            max_sessions_per_source: r.max_sessions_per_source,
            max_members_per_source: r.max_members_per_source,
            stats_interval_s: 30,
        }
    }
}

impl EmbeddedRelay {
    /// The workers the relay runs (`workers`, 0: half the processors, at least one).
    pub fn worker_count(&self) -> usize {
        match self.workers {
            0 => kalamar_relay::server::auto_workers().div_ceil(2).max(1),
            n => n,
        }
    }

    /// The relay's own configuration: `kalamar-relay`'s defaults with these limits and `secret`.
    pub fn relay_config(
        &self,
        secret: Option<kalamar_relay::roomauth::RoomSecret>,
    ) -> kalamar_relay::relay::RelayConfig {
        kalamar_relay::relay::RelayConfig {
            max_rooms: self.max_rooms,
            max_sessions: self.max_sessions,
            max_sessions_per_source: self.max_sessions_per_source,
            max_members_per_source: self.max_members_per_source,
            room_secret: secret,
            ..Default::default()
        }
    }
}

/// `[relay_load]` (protocol 1.1.13, `relay.md` 4.3): the service asks each relay bound to it (a `[[relays]]` entry
/// with a secret) for its load, and a new match goes to a relay with room: among the relays within
/// `latency_tolerance_ms` of the best one for the match's slowest player, the one with the most room left. A relay at
/// `full_at_percent` of its capacity takes no new match until it is back under `resume_at_percent`; one that stops
/// answering takes none while another one has room. Relays that announce nothing (open, or not knowing the query)
/// are chosen without their load.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RelayLoadSection {
    /// Seconds between two questions to each bound relay; 0: never asked (relays are chosen without their load).
    pub status_s: u64,
    /// Relays whose round trip for the match's slowest player is within this many milliseconds of the best one are
    /// equally good in latency; the one with more room left then wins.
    pub latency_tolerance_ms: u32,
    /// Load (the larger of sessions and rooms against their caps, and the workers' mean time spent working), in
    /// percent, at which a relay takes no new match ...
    pub full_at_percent: u8,
    /// ... until it is back at or under this one (hysteresis: a relay near the limit does not take and refuse matches
    /// in turn).
    pub resume_at_percent: u8,
}

impl Default for RelayLoadSection {
    fn default() -> Self {
        RelayLoadSection {
            status_s: 5,
            latency_tolerance_ms: 30,
            full_at_percent: 90,
            resume_at_percent: 75,
        }
    }
}

/// `path` made absolute against the current directory and without `.` or `..`, without touching the disk:
/// `data/relay.key`, `./data/relay.key` and `<current directory>/data/relay.key` are the same file.
pub fn normalize_path(path: &Path) -> PathBuf {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Whether two configured paths name the same file (see [`normalize_path`]).
pub fn same_file(a: &Path, b: &Path) -> bool {
    normalize_path(a) == normalize_path(b)
}

impl RelayEntry {
    /// `host:port`, what a match's game block names.
    pub fn addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
    /// The relay's id: the configured one, else `host:port`.
    pub fn key(&self) -> String {
        self.id.clone().unwrap_or_else(|| self.addr())
    }
}

fn default_relay_selection() -> String {
    "latency".into()
}
fn default_relay_probe() -> u64 {
    30
}

fn default_relay_port() -> u16 {
    47313
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyEntry {
    pub id: String,
    #[serde(default)]
    pub name: Text,
    #[serde(default)]
    pub summary: Text,
    pub quorum: Quorum,
    pub silence: Silence,
    pub disagreement: Disagreement,
    pub rated_requires_both_teams: bool,
    #[serde(default = "abort_void")]
    pub abort: Abort,
    #[serde(default = "disconnect_game")]
    pub disconnect: Disconnect,
    #[serde(default = "no_show_void")]
    pub no_show: NoShow,
    pub accept_s: u32,
    pub launch_s: u32,
    pub result_s: u32,
    pub grace_s: u32,
    /// 1.1.1: check `rules.loadout` against the record (`ignore`, `void`, `forfeit`).
    #[serde(default)]
    pub loadout_check: LoadoutCheck,
    /// 1.1: `auto`, `referee` or `auto_then_referee` (a human decides through the operator API).
    #[serde(default)]
    pub decider: Decider,
    /// How long a match may wait for a referee before the automatic decision applies.
    #[serde(default = "default_review")]
    pub review_deadline_s: u32,
    /// 1.1.7: what the spectators' reports are used for: `ignore`, `tiebreak` (they break a disagreement between
    /// players) or `required` (also: a rated result changes ratings only when a counted witness confirms it, and a
    /// counted witness who contradicts the players voids the match).
    #[serde(default)]
    pub witness: WitnessUse,
    /// 1.1.7: whose reports count as a witness's: `any` spectator or only `referees`.
    #[serde(default)]
    pub witness_from: WitnessFrom,
    /// 1.1.8: what the reports' counters (the teams' scores and each player's statistics) weigh: `flag` (default:
    /// compared with a wide tolerance, a larger gap flagged in the outcome and the log, never voiding), `ignore`, or
    /// `strict` (every counter equal: real matches between two machines are then void, each game computing its own).
    #[serde(default = "stats_flag")]
    pub stats: StatsCheck,
    /// 1.1.11: what a player's voluntary departure from a match in play does when the match has no valid result:
    /// `void` (the default), `forfeit` (the leaver's team loses, rated as any result) or
    /// `forfeit_unrated` (it loses, no rating changes). Only a departure the leaver's own launcher reports in play
    /// counts; a crash or a lost connection stays void (`docs/protocol/leaving.md`).
    #[serde(default)]
    pub leave: Leave,
}

fn stats_flag() -> StatsCheck {
    StatsCheck::Flag
}

fn default_review() -> u32 {
    3600
}

fn abort_void() -> Abort {
    Abort::Void
}
fn disconnect_game() -> Disconnect {
    Disconnect::GameResult
}
fn no_show_void() -> NoShow {
    NoShow::Void
}

impl PolicyEntry {
    pub fn policy(&self) -> Policy {
        Policy {
            quorum: self.quorum,
            silence: self.silence,
            disagreement: self.disagreement,
            rated_requires_both_teams: self.rated_requires_both_teams,
            abort: self.abort,
            disconnect: self.disconnect,
            no_show: self.no_show,
            deadlines: Deadlines {
                accept_s: self.accept_s,
                launch_s: self.launch_s,
                result_s: self.result_s,
                grace_s: self.grace_s,
            },
            loadout_check: self.loadout_check,
            decider: self.decider,
            witness: self.witness,
            witness_from: self.witness_from,
            stats: self.stats,
            leave: self.leave,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RoomsSection {
    pub teams_choices: Vec<[usize; 2]>,
    pub default_teams: [usize; 2],
    /// Mode ids offered (the first is the default unless `default_mode` is set). One mode: no mode field.
    pub modes: Vec<String>,
    pub default_mode: Option<String>,
    /// Stage ids offered besides "the game's choice". Empty: no stage field.
    pub stages: Vec<String>,
    pub password: bool,
    pub min_players: usize,
    pub result_policy: String,
    pub idle_close_s: i64,
    pub invites: bool,
    /// Invitations to rooms and parties need a friendship (when `friends` is on).
    pub invites_friends_only: bool,
    /// `owner` (the room owner hosts the game session) or `random` (drawn among the players whose games host well,
    /// `crate::host`).
    pub host: String,
    /// Loadouts offered in the room form (`loadout` field); empty: no such field.
    pub loadouts: Vec<LoadoutPreset>,
    /// 1.1.6: drafts offered in the room form (`draft` field, `[[drafts]]` ids); empty: no such field.
    pub drafts: Vec<String>,
    /// 1.1.7: places in the stands a room may open (`spectators` and `spectators_who` fields).
    pub spectators: RoomStands,
}

impl Default for RoomsSection {
    fn default() -> Self {
        RoomsSection {
            teams_choices: vec![[1, 1], [2, 2], [3, 3], [4, 4]],
            default_teams: [4, 4],
            modes: vec!["turf".into()],
            default_mode: None,
            stages: Vec::new(),
            password: true,
            min_players: 2,
            result_policy: "casual".into(),
            idle_close_s: 7200,
            invites: true,
            invites_friends_only: true,
            host: "owner".into(),
            loadouts: Vec::new(),
            drafts: Vec::new(),
            spectators: RoomStands::default(),
        }
    }
}

impl RoomsSection {
    pub fn default_mode(&self) -> &str {
        self.default_mode
            .as_deref()
            .or(self.modes.first().map(String::as_str))
            .unwrap_or("turf")
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolEntry {
    pub mode: String,
    #[serde(default)]
    pub stage: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rotation {
    pub period_s: i64,
    pub pools: Vec<Vec<PoolEntry>>,
}

/// A weekly opening window in UTC: `days` (`mon`..`sun`), `from` and `to` as `HH:MM` (`to` may be `24:00`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowEntry {
    pub days: Vec<String>,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SearchWindow {
    /// Rating gap accepted at once, widened per second of waiting, up to a maximum.
    pub base: f64,
    pub widen_per_s: f64,
    pub max: f64,
}

impl Default for SearchWindow {
    fn default() -> Self {
        SearchWindow {
            base: 150.0,
            widen_per_s: 10.0,
            max: 1500.0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueEntry {
    pub id: String,
    pub name: Text,
    #[serde(default)]
    pub description: Text,
    #[serde(default)]
    pub order: i64,
    pub teams: [usize; 2],
    #[serde(default)]
    pub min_teams: Option<[usize; 2]>,
    /// The oldest ticket's wait before smaller teams are tried.
    #[serde(default = "default_shrink")]
    pub shrink_after_s: i64,
    #[serde(default)]
    pub rated: bool,
    #[serde(default)]
    pub ladder: Option<String>,
    #[serde(default = "one")]
    pub party_min: usize,
    #[serde(default = "one")]
    pub party_max: usize,
    /// `random`, `rotation`, `fixed` or `game`.
    #[serde(default = "default_selection")]
    pub selection: String,
    pub pool: Vec<PoolEntry>,
    #[serde(default)]
    pub rotation: Option<Rotation>,
    pub result_policy: String,
    #[serde(default)]
    pub schedule: Vec<WindowEntry>,
    /// `placement_done:<ladder>` or `min_games:<ladder>:<n>`.
    #[serde(default)]
    pub requirements: Vec<String>,
    #[serde(default)]
    pub search: SearchWindow,
    /// Ladder whose ratings balance an unrated queue's teams (default: the queue's own ladder, else none).
    #[serde(default)]
    pub balance_ladder: Option<String>,
    #[serde(default = "yes")]
    pub show_searching: bool,
    /// Weapon and gear constraints of the queue's matches (the JSON of `rules.loadout`).
    #[serde(default)]
    pub loadout: Option<serde_json::Value>,
    /// 1.1.6: the stage draft of the queue's matches, after every player accepted (a `[[drafts]]` id).
    #[serde(default)]
    pub draft: Option<String>,
    /// 1.1.7: places in the stands of the queue's matches (taken before the launch: during the accept phase or the
    /// draft).
    #[serde(default)]
    pub spectators: Option<Stands>,
}

fn default_shrink() -> i64 {
    90
}
fn one() -> usize {
    1
}
fn default_selection() -> String {
    "random".into()
}

impl QueueEntry {
    pub fn min_teams(&self) -> [usize; 2] {
        self.min_teams.unwrap_or(self.teams)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PenaltiesSection {
    pub counted: Vec<String>,
    pub window_s: i64,
    pub cooldowns_s: Vec<i64>,
    pub summary: Text,
}

impl Default for PenaltiesSection {
    fn default() -> Self {
        PenaltiesSection {
            counted: vec![
                "decline".into(),
                "accept_timeout".into(),
                "no_show".into(),
                "left".into(),
            ],
            window_s: 86400,
            cooldowns_s: vec![0, 60, 300, 900, 3600],
            summary: en(
                "Declining or missing a found match, not launching in time or leaving a queue match blocks the \
                         queues: the first time is a warning, then 1, 5, 15 and 60 minutes within a day.",
            ),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LadderEntry {
    pub id: String,
    pub name: Text,
    #[serde(default)]
    pub description: Text,
    #[serde(default = "yes")]
    pub leaderboard: bool,
}

/// The reference rating system: Glicko-2 per ladder and season, placement, tiers over a conservative rating.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RatingSection {
    pub initial_rating: f64,
    pub initial_deviation: f64,
    pub initial_volatility: f64,
    pub tau: f64,
    pub min_deviation: f64,
    pub placement_games: i64,
    /// Shown value = rating - `display_deviations` x deviation.
    pub display_deviations: f64,
    /// At a season roll-over: new rating = initial + (old - initial) x `season_keep`; deviation at least
    /// `season_deviation_floor`.
    pub season_keep: f64,
    pub season_deviation_floor: f64,
    pub tiers: Vec<TierEntry>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TierEntry {
    pub id: String,
    pub name: Text,
    pub color: String,
    /// Lowest shown value of the tier (the first tier's is ignored).
    pub min: f64,
}

impl Default for RatingSection {
    fn default() -> Self {
        let tier = |id: &str, name: &str, color: &str, min: f64| TierEntry {
            id: id.into(),
            name: en(name),
            color: color.into(),
            min,
        };
        RatingSection {
            initial_rating: 1500.0,
            initial_deviation: 350.0,
            initial_volatility: 0.06,
            tau: 0.5,
            min_deviation: 45.0,
            placement_games: 5,
            display_deviations: 2.0,
            season_keep: 0.5,
            season_deviation_floor: 250.0,
            tiers: vec![
                tier("rookie", "Rookie", "#8C8C8C", f64::NEG_INFINITY),
                tier("bronze", "Bronze", "#B0703C", 900.0),
                tier("silver", "Silver", "#A8B0B8", 1150.0),
                tier("gold", "Gold", "#D4AF37", 1350.0),
                tier("platinum", "Platinum", "#4FB3BF", 1550.0),
                tier("diamond", "Diamond", "#5B8DEF", 1750.0),
                tier("master", "Master", "#9B59B6", 1950.0),
            ],
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Catalog {
    pub modes: Vec<CatalogEntry>,
    pub stages: Vec<CatalogEntry>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogEntry {
    pub id: String,
    pub label: Text,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModerationSection {
    pub report_categories: Vec<CatalogEntry>,
}

impl Default for ModerationSection {
    fn default() -> Self {
        let c = |id: &str, l: &str| CatalogEntry {
            id: id.into(),
            label: en(l),
        };
        ModerationSection {
            report_categories: vec![
                c("cheating", "Cheating"),
                c("harassment", "Harassment"),
                c("name", "Offensive name"),
                c("griefing", "Leaving or sabotaging matches"),
                c("other", "Something else"),
            ],
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RulesSection {
    pub summary: Text,
    pub sections: Vec<RulesSectionEntry>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulesSectionEntry {
    pub id: String,
    pub title: Text,
    pub body: Text,
}

impl Default for RulesSection {
    fn default() -> Self {
        RulesSection {
            summary: en(
                "Be kind. One account per person. Results are reported by your launcher and compared with the \
                         other players' reports.",
            ),
            sections: vec![RulesSectionEntry {
                id: "fair-play".into(),
                title: en("Fair play"),
                body: en(
                    "Every player's launcher uploads the game's record of the match and the server compares \
                          them. One player alone cannot change a result; players who agree on a false result can, \
                          and a modified game can lie to everyone. Report cheating: moderators review reports.",
                ),
            }],
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EventsSection {
    pub heartbeat_s: u64,
    pub resume_window_s: i64,
    pub buffer: usize,
    pub max_connections_per_user: usize,
}

impl Default for EventsSection {
    fn default() -> Self {
        EventsSection {
            heartbeat_s: 20,
            resume_window_s: 120,
            buffer: 256,
            max_connections_per_user: 4,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Retention {
    /// Raw result and status files of ended matches are deleted after this many days (the decision is kept).
    pub raw_reports_days: i64,
    /// Closed moderation reports are deleted after this many days.
    pub closed_reports_days: i64,
    /// Security review M6: how long, at most, the login identity of an erased account still carries the open reports
    /// about it (a running cooldown and the offenses of the penalty window are kept for as long as they run).
    pub erased_hold_days: i64,
    /// Administrative actions (`audit`: who did what to which account id) are deleted after this many days.
    pub audit_days: i64,
}

impl Default for Retention {
    fn default() -> Self {
        Retention {
            raw_reports_days: 30,
            closed_reports_days: 365,
            erased_hold_days: 30,
            audit_days: 365,
        }
    }
}

/// The built-in result policies used when the configuration declares none.
pub fn builtin_policies() -> Vec<PolicyEntry> {
    let standard = Policy::standard();
    vec![
        PolicyEntry {
            id: "standard".into(),
            name: en("Standard"),
            summary: en(
                "A result counts when more than half of the players back it: players whose result agrees and \
                         players who played and sent nothing (a player whose game failed is left out of the count when \
                         its own team recorded it leaving; otherwise its error voids the match). When results differ, \
                         the strict majority wins, else the match is void. Ratings change only if each team confirmed \
                         the result with a report of its own.",
            ),
            quorum: standard.quorum,
            silence: standard.silence,
            disagreement: standard.disagreement,
            rated_requires_both_teams: true,
            abort: Abort::Void,
            disconnect: Disconnect::GameResult,
            no_show: NoShow::Void,
            accept_s: 20,
            launch_s: 90,
            result_s: 2400,
            grace_s: 120,
            loadout_check: LoadoutCheck::Ignore,
            decider: Decider::Auto,
            review_deadline_s: 3600,
            witness: WitnessUse::Ignore,
            witness_from: WitnessFrom::Any,
            stats: StatsCheck::Flag,
            leave: Leave::Void,
        },
        PolicyEntry {
            id: "casual".into(),
            name: en("Casual"),
            summary: en("The first result received counts unless another player's result contradicts it."),
            quorum: Quorum::Any,
            silence: Silence::Ignore,
            disagreement: Disagreement::Void,
            rated_requires_both_teams: false,
            abort: Abort::Void,
            disconnect: Disconnect::GameResult,
            no_show: NoShow::Void,
            accept_s: 20,
            launch_s: 120,
            result_s: 2400,
            grace_s: 60,
            loadout_check: LoadoutCheck::Ignore,
            decider: Decider::Auto,
            review_deadline_s: 3600,
            witness: WitnessUse::Ignore,
            witness_from: WitnessFrom::Any,
            stats: StatsCheck::Flag,
            leave: Leave::Void,
        },
    ]
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Config::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// A configuration from its TOML text, normalized and validated.
    pub fn parse(text: &str) -> Result<Config, String> {
        let mut cfg: Config = toml::from_str(text).map_err(|e| e.to_string())?;
        cfg.normalize();
        let errors = cfg.validate();
        if errors.is_empty() {
            Ok(cfg)
        } else {
            Err(errors.join("\n"))
        }
    }

    /// For a reload (`reload.rs`): what only a restart can change is taken from the `running` configuration; returns
    /// the names of those parts that differ in `self`, for the operator.
    pub fn keep_restart_only(&mut self, running: &Config) -> Vec<&'static str> {
        let mut kept = Vec::new();
        macro_rules! keep {
            ($name:literal, $($field:tt)+) => {
                if format!("{:?}", self.$($field)+) != format!("{:?}", running.$($field)+) {
                    self.$($field)+ = running.$($field)+.clone();
                    kept.push($name);
                }
            };
        }
        keep!("server.api_url", server.api_url);
        keep!("server.bind", server.bind);
        keep!("server.production", server.production);
        keep!("server.database", server.database);
        keep!("server.secret_file", server.secret_file);
        keep!("server.trusted_proxies", server.trusted_proxies);
        keep!("features.test_clock", features.test_clock);
        keep!("auth", auth);
        keep!("rate_limits", rate_limits);
        keep!("webhooks", webhooks);
        // The relay this service runs changes with a restart only; so do the `[[relays]]` while it waits for one,
        // or new matches could be sent to a port where nothing listens yet (or to a relay with the wrong secret).
        let embedded_waits = format!("{:?}", self.embedded_relay) != format!("{:?}", running.embedded_relay);
        keep!("embedded_relay", embedded_relay);
        if embedded_waits {
            keep!("relays (they wait with embedded_relay)", relays);
        }
        kept
    }

    /// Adds the built-in policies the configuration does not redefine, orders the queues, and gives the `[[relays]]`
    /// entries that name the embedded relay its secret when they name none.
    pub fn normalize(&mut self) {
        // The built-in policies (`standard`, `casual`) are available unless the configuration redefines their id.
        for b in builtin_policies() {
            if !self.result_policies.iter().any(|p| p.id == b.id) {
                self.result_policies.push(b);
            }
        }
        self.queues.sort_by_key(|q| q.order);
        if let Some(f) = self
            .embedded_relay
            .secret_file
            .clone()
            .filter(|_| self.embedded_relay.enabled)
        {
            for i in 0..self.relays.len() {
                let r = &self.relays[i];
                if r.secret_file.is_none() && r.secret_env.is_none() && self.names_embedded(r) {
                    let note = format!(
                        "relays.{}: names the relay this service runs; its secret is embedded_relay.secret_file ({})",
                        r.key(),
                        f.display()
                    );
                    self.relays[i].secret_file = Some(f.clone());
                    self.derived.push(note);
                }
            }
        }
    }

    /// The host part of `server.api_url` (`play.example.org`), lower case.
    pub fn api_host(&self) -> String {
        let rest = self.server.api_url.split_once("://").map(|(_, r)| r).unwrap_or("");
        let host = rest.split(['/', '?']).next().unwrap_or("");
        let host = match host.strip_prefix('[') {
            Some(v6) => v6.split(']').next().unwrap_or(""),
            None => host.rsplit_once(':').map_or(host, |(h, _)| h),
        };
        host.to_ascii_lowercase()
    }

    /// Whether the `[[relays]]` entry `r` names the relay of `[embedded_relay]`: its port with a host of this machine
    /// (a loopback address or `localhost`, the address the relay listens on, or the host of `server.api_url`), or
    /// the only entry there is (an embedded relay no entry names would serve nobody).
    pub fn names_embedded(&self, r: &RelayEntry) -> bool {
        let er = &self.embedded_relay;
        if !er.enabled {
            return false;
        }
        if self.relays.len() == 1 {
            return true;
        }
        let h = r.host.trim_end_matches('.').to_ascii_lowercase();
        let local = h == "localhost"
            || h.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
            || (h == er.bind && er.bind.parse::<std::net::IpAddr>().is_ok_and(|ip| !ip.is_unspecified()))
            || h == self.api_host();
        r.port == er.port && local
    }

    pub fn bind_addr(&self) -> Result<SocketAddr, String> {
        self.server.bind.parse().map_err(|e| format!("server.bind: {e}"))
    }

    /// The path the API is mounted at (`/api` for `https://host/api`), without a trailing slash; `""` for the root.
    pub fn api_prefix(&self) -> String {
        let rest = self.server.api_url.split_once("://").map(|(_, r)| r).unwrap_or("");
        match rest.find('/') {
            Some(i) => rest[i..].trim_end_matches('/').to_string(),
            None => String::new(),
        }
    }

    pub fn policy(&self, id: &str) -> Option<&PolicyEntry> {
        self.result_policies.iter().find(|p| p.id == id)
    }
    pub fn queue(&self, id: &str) -> Option<&QueueEntry> {
        self.queues.iter().find(|q| q.id == id)
    }
    pub fn ladder(&self, id: &str) -> Option<&LadderEntry> {
        self.ladders.iter().find(|l| l.id == id)
    }
    /// A draft preset (`[[drafts]]`), when the area is on.
    pub fn draft(&self, id: &str) -> Option<&DraftPreset> {
        self.drafts.iter().find(|d| d.id == id).filter(|_| self.features.draft)
    }
    pub fn default_relay(&self) -> Option<String> {
        self.relays.first().map(RelayEntry::addr)
    }

    /// Every problem with the configuration (empty when it can run).
    pub fn validate(&self) -> Vec<String> {
        let mut e = Vec::new();
        let s = &self.server;
        let f = &self.features;
        if s.name.trim().is_empty() || s.name.chars().count() > 64 {
            e.push("server.name: 1 to 64 characters".into());
        }
        let rl = &self.rate_limits;
        if !(16..=32).contains(&rl.ipv4_prefix) || !(32..=128).contains(&rl.ipv6_prefix) {
            e.push("rate_limits: ipv4_prefix is 16 to 32, ipv6_prefix 32 to 128".into());
        }
        if rl.accounts_per_ip_per_day < 0.0 || rl.room_starts_per_hour < 0.0 {
            e.push("rate_limits: accounts_per_ip_per_day and room_starts_per_hour are 0 (no limit) or more".into());
        }
        let local = |u: &str| {
            ["http://127.0.0.1", "http://localhost", "http://[::1]"]
                .iter()
                .any(|p| u.starts_with(p))
        };
        if !(s.api_url.starts_with("https://") || local(&s.api_url)) || s.api_url.len() > 200 {
            e.push("server.api_url: an https URL of at most 200 characters (http only on loopback)".into());
        }
        if s.production && local(&s.api_url) {
            e.push("server.api_url: a production server needs a public https URL".into());
        }
        if self.bind_addr().is_err() {
            e.push("server.bind: not an address:port".into());
        }
        for p in &s.trusted_proxies {
            if crate::netutil::Cidr::parse(p).is_none() {
                e.push(format!("server.trusted_proxies: `{p}` is not a CIDR"));
            }
        }
        if s.operator.name.is_empty() || s.operator.contact.is_empty() {
            e.push("server.operator: name and contact are required".into());
        }
        if self.relays.is_empty() {
            e.push("relays: at least one relay is required (matches need one)".into());
        }
        let mut relay_keys = std::collections::HashSet::new();
        for r in &self.relays {
            if r.host.is_empty() || r.host.len() > 253 || r.port == 0 {
                e.push("relays: host and a non-zero port".into());
            }
            if r.id.as_ref().is_some_and(|i| !crate::util::is_id(i)) {
                e.push(format!("relays: bad id `{}`", r.id.as_deref().unwrap_or_default()));
            }
            if !relay_keys.insert(r.key()) {
                e.push(format!("relays: `{}` is listed twice", r.key()));
            }
        }
        let er = &self.embedded_relay;
        if er.enabled && (er.bind.parse::<std::net::IpAddr>().is_err() || er.port == 0) {
            e.push("embedded_relay: bind is an IP address (0.0.0.0), port is not 0".into());
        }
        if er.enabled
            && let Err(m) = er.relay_config(None).validate()
        {
            e.push(format!("embedded_relay: {m}"));
        }
        if er.workers > 256 {
            e.push("embedded_relay.workers: 0 (half the processors) to 256".into());
        }
        self.validate_embedded_secret(&mut e);
        if self.relays.len() > 16 {
            e.push("relays: at most 16".into());
        }
        if !matches!(s.relay_selection.as_str(), "latency" | "least_busy" | "first") {
            e.push("server.relay_selection: latency, least_busy or first".into());
        }
        if s.relay_probe_s != 0 && !(5..=3600).contains(&s.relay_probe_s) {
            e.push("server.relay_probe_s: 0 (off) or 5 to 3600".into());
        }
        let rl = &self.relay_load;
        if rl.status_s != 0 && !(2..=600).contains(&rl.status_s) {
            e.push("relay_load.status_s: 0 (off) or 2 to 600".into());
        }
        if rl.latency_tolerance_ms > 1000 {
            e.push("relay_load.latency_tolerance_ms: 0 to 1000".into());
        }
        if !(1..=100).contains(&rl.full_at_percent) || rl.resume_at_percent >= rl.full_at_percent {
            e.push("relay_load: full_at_percent 1 to 100, and resume_at_percent below it".into());
        }
        if s.production && (f.test_clock || self.auth.dev.enabled) {
            e.push("a production server refuses the dev provider and the test clock (server.production)".into());
        }
        self.validate_auth(&mut e);
        if f.presence && !f.friends {
            e.push("features.presence needs features.friends".into());
        }
        if f.leaderboards && !f.ratings {
            e.push("features.leaderboards needs features.ratings".into());
        }
        if !f.rooms && !f.queues {
            e.push("features: enable rooms or queues (matches need one of them)".into());
        }
        if f.parties && !f.queues {
            e.push("features.parties needs features.queues".into());
        }
        let l = &self.limits;
        if l.display_name_min < 1 || l.display_name_max < l.display_name_min || l.display_name_max > 32 {
            e.push("limits.display_name_min/max: 1 <= min <= max <= 32".into());
        }
        if l.party_size < 2 || l.party_size > MAX_PLAYERS {
            e.push("limits.party_size: 2 to 8".into());
        }
        if l.body_bytes < 16 * 1024 || l.result_bytes < 64 * 1024 || l.status_bytes < 4 * 1024 {
            e.push(
                "limits: body_bytes >= 16384, result_bytes >= 65536, status_bytes >= 4096 (protocol minimums)".into(),
            );
        }
        let mut ids = std::collections::HashSet::new();
        for p in &self.result_policies {
            if !ids.insert(p.id.as_str()) {
                e.push(format!("result_policies: duplicate id `{}`", p.id));
            }
            if !crate::util::is_id(&p.id) {
                e.push(format!("result_policies: bad id `{}`", p.id));
            }
            let pol = p.policy();
            if !pol.is_known() || p.decider == Decider::Unknown {
                e.push(format!("result_policies.{}: unknown value", p.id));
            }
            if pol.disagreement == Disagreement::Rematch
                || pol.abort == Abort::Rematch
                || pol.no_show == NoShow::Rematch
            {
                e.push(format!(
                    "result_policies.{}: `rematch` is not implemented by this server",
                    p.id
                ));
            }
            if p.launch_s < 10 || p.result_s < 60 || p.accept_s < 5 {
                e.push(format!("result_policies.{}: deadlines too short", p.id));
            }
        }
        if f.rooms {
            let r = &self.rooms;
            if self.policy(&r.result_policy).is_none() {
                e.push(format!("rooms.result_policy: unknown policy `{}`", r.result_policy));
            }
            if r.teams_choices.is_empty() || !r.teams_choices.contains(&r.default_teams) {
                e.push("rooms.teams_choices must include rooms.default_teams".into());
            }
            for t in &r.teams_choices {
                if t[0] < 1 || t[1] < 1 || t[0] + t[1] > MAX_PLAYERS {
                    e.push(format!("rooms.teams_choices: {t:?} (each team 1+, at most 8 players)"));
                }
            }
            if r.modes.is_empty() {
                e.push("rooms.modes: at least one mode id".into());
            }
            if r.default_mode.as_ref().is_some_and(|m| !r.modes.contains(m)) {
                e.push("rooms.default_mode must be one of rooms.modes".into());
            }
            if r.min_players < 2 {
                e.push("rooms.min_players: at least 2".into());
            }
            if r.host != "owner" && r.host != "random" {
                e.push("rooms.host: `owner` or `random`".into());
            }
        }
        if f.queues && self.queues.is_empty() {
            e.push("features.queues needs at least one [[queues]]".into());
        }
        let mut qids = std::collections::HashSet::new();
        for q in &self.queues {
            let n = format!("queues.{}", q.id);
            if !qids.insert(q.id.as_str()) || !crate::util::is_id(&q.id) {
                e.push(format!("{n}: duplicate or bad id"));
            }
            let min = q.min_teams();
            if q.teams[0] < 1 || q.teams[1] < 1 || q.teams[0] + q.teams[1] > MAX_PLAYERS {
                e.push(format!("{n}.teams: each team 1+, at most 8 players"));
            }
            if min[0] < 1 || min[1] < 1 || min[0] > q.teams[0] || min[1] > q.teams[1] {
                e.push(format!("{n}.min_teams: between [1, 1] and teams"));
            }
            if q.teams[0] - min[0] != q.teams[1] - min[1] {
                e.push(format!("{n}.min_teams: both teams shrink by the same number"));
            }
            if q.party_min < 1 || q.party_max < q.party_min || q.party_max > q.teams[0].max(q.teams[1]) {
                e.push(format!("{n}: 1 <= party_min <= party_max <= the larger team"));
            }
            if q.party_max > 1 && !f.parties {
                e.push(format!("{n}: party_max > 1 needs features.parties"));
            }
            if self.policy(&q.result_policy).is_none() {
                e.push(format!("{n}.result_policy: unknown policy"));
            }
            if q.rated {
                match &q.ladder {
                    Some(l) if self.ladder(l).is_some() => {}
                    _ => e.push(format!("{n}: a rated queue needs a configured ladder")),
                }
                if !f.ratings {
                    e.push(format!("{n}: rated queues need features.ratings"));
                }
            }
            if !matches!(q.selection.as_str(), "random" | "rotation" | "fixed" | "game") {
                e.push(format!("{n}.selection: random, rotation, fixed or game"));
            }
            if q.pool.is_empty() {
                e.push(format!("{n}.pool: at least one entry"));
            }
            if q.selection == "rotation" {
                match &q.rotation {
                    Some(r) if r.period_s >= 60 && !r.pools.is_empty() && r.pools.iter().all(|p| !p.is_empty()) => {}
                    _ => e.push(format!("{n}.rotation: period_s >= 60 and non-empty pools")),
                }
            }
            for w in &q.schedule {
                if crate::util::parse_window(w).is_none() {
                    e.push(format!("{n}.schedule: bad window (days mon..sun, HH:MM)"));
                }
            }
            for r in &q.requirements {
                if crate::matchmaking::Requirement::parse(r).is_none() {
                    e.push(format!(
                        "{n}.requirements: `{r}` (placement_done:<ladder> or min_games:<ladder>:<n>)"
                    ));
                }
            }
        }
        let mut lids = std::collections::HashSet::new();
        for l in &self.ladders {
            if !lids.insert(l.id.as_str()) || !crate::util::is_id(&l.id) {
                e.push(format!("ladders: duplicate or bad id `{}`", l.id));
            }
        }
        if f.ratings && self.ladders.is_empty() {
            e.push("features.ratings needs at least one [[ladders]]".into());
        }
        if self.rating.tiers.is_empty() || self.rating.placement_games < 0 {
            e.push("rating: at least one tier, placement_games >= 0".into());
        }
        if self.penalties.cooldowns_s.is_empty() || self.penalties.window_s < 60 {
            e.push("penalties: at least one cooldown, window_s >= 60".into());
        }
        crate::activities::validate(self, &mut e);
        crate::badges::validate(self, &mut e);
        crate::draft::validate(self, &mut e);
        crate::spectate::validate(self, &mut e);
        for w in &self.webhooks {
            let local = w.url.starts_with("http://127.0.0.1");
            if !(w.url.starts_with("https://") || (local && !s.production)) || w.url.len() > 500 {
                e.push(format!(
                    "webhooks: `{}` must be https (http://127.0.0.1 only off production)",
                    w.url
                ));
            }
            if w.secret.is_none() && w.secret_file.is_none() {
                e.push("webhooks: a secret or secret_file is required".into());
            }
        }
        for t in &self.operator_tokens {
            if t.sha256.len() != 64 || crate::util::unhex(&t.sha256).is_none() {
                e.push(format!("operator_tokens.{}: sha256 is 64 hex digits", t.name));
            }
            if t.scopes
                .iter()
                .any(|x| !matches!(x.as_str(), "read" | "results" | "moderation" | "activities"))
            {
                e.push(format!(
                    "operator_tokens.{}: scopes are read, results, moderation, activities",
                    t.name
                ));
            }
        }
        if self.events.heartbeat_s < 5 || self.events.buffer < 16 || self.events.max_connections_per_user < 1 {
            e.push("events: heartbeat_s >= 5, buffer >= 16, max_connections_per_user >= 1".into());
        }
        e
    }

    /// The login providers: at least one, unique ids, every OAuth provider complete and on https, a sound password
    /// configuration.
    fn validate_auth(&self, e: &mut Vec<String>) {
        let a = &self.auth;
        let production = self.server.production;
        let oauth = crate::auth::oauth::configured(self);
        let password = a.password.as_ref().filter(|p| p.enabled);
        if !a.dev.enabled && !a.guest.enabled && oauth.is_empty() && password.is_none() {
            e.push("auth: enable at least one provider".into());
        }
        let mut ids: Vec<String> = Vec::new();
        if a.guest.enabled {
            ids.push("guest".into());
        }
        if a.dev.enabled {
            ids.push("dev".into());
        }
        if password.is_some() {
            ids.extend(crate::auth::password::IDS.iter().map(|s| s.to_string()));
        }
        for r in &oauth {
            match r {
                Err(m) => e.push(m.clone()),
                Ok(r) => {
                    e.extend(crate::auth::oauth::check(r, production));
                    if ids.contains(&r.id) {
                        e.push(format!(
                            "auth: two login providers use the id `{}` (guest, dev and the password- forms are taken)",
                            r.id
                        ));
                    }
                    ids.push(r.id.clone());
                }
            }
        }
        let Some(p) = password else { return };
        if !a.guest.enabled && !a.dev.enabled && oauth.is_empty() {
            e.push(
                "auth.password: the protocol forbids requiring an e-mail address to play (overview.md, section 4): \
                 also enable [auth.guest] or an OAuth provider"
                    .into(),
            );
        }
        if !(8..=64).contains(&p.min_length) || p.max_length < p.min_length.max(64) || p.max_length > 256 {
            e.push("auth.password: 8 <= min_length <= 64, max(min_length, 64) <= max_length <= 256".into());
        }
        if !crate::auth::password::argon2_bounds(&p.argon2) {
            e.push(
                "auth.password.argon2: 1 <= parallelism <= 16, 1 <= iterations <= 20, 8 x parallelism <= memory_kib <= 1048576"
                    .into(),
            );
        }
        if production && !crate::auth::password::argon2_strong(&p.argon2) {
            e.push(
                "auth.password.argon2: below the OWASP minimum (memory_kib >= 7168 and memory_kib x iterations >= 35840; \
                 the default is 19456 x 2)"
                    .into(),
            );
        }
        if !(1..=64).contains(&p.max_concurrent_hashes) {
            e.push("auth.password.max_concurrent_hashes: 1 to 64".into());
        }
        if !(300..=86_400).contains(&p.email_code_ttl_s) || !(300..=86_400).contains(&p.operator_code_ttl_s) {
            e.push("auth.password: email_code_ttl_s and operator_code_ttl_s between 300 and 86400".into());
        }
        let l = &p.lockout;
        if !(1.0..=1000.0).contains(&l.account_failures)
            || !(1.0..=1000.0).contains(&l.ip_failures)
            || l.account_refill_s < 1.0
            || l.ip_refill_s < 1.0
            || !(0..=365).contains(&l.known_address_days)
            || !(1.0..=100.0).contains(&p.codes_per_hour)
        {
            e.push(
                "auth.password.lockout: failures 1 to 1000, refill_s >= 1, known_address_days 0 to 365; codes_per_hour 1 to 100"
                    .into(),
            );
        }
        if let Some(s) = &p.smtp {
            if s.host.is_empty() || s.host.len() > 253 || (s.host.contains(['/', ' ', ':']) && s.host != "::1") {
                e.push("auth.password.smtp.host: a host name".into());
            }
            if let Err(m) = crate::auth::mail::tls_ok(s, production) {
                e.push(m);
            }
            if !crate::auth::mail::sender_ok(&s.from) {
                e.push("auth.password.smtp.from: `Name <address>` or `address`".into());
            }
            if s.username.is_some() && s.password_file.is_none() && s.password_env.is_none() {
                e.push("auth.password.smtp: with a username, set password_file or password_env".into());
            }
            if !(1..=120).contains(&s.timeout_s) {
                e.push("auth.password.smtp.timeout_s: 1 to 120".into());
            }
        }
    }

    /// A relay bound by a secret refuses every room code sealed with another one (`JOIN_NAK` 3): the relay this
    /// service runs must be named in `[[relays]]` with its own secret, or every match sent there would fail.
    fn validate_embedded_secret(&self, e: &mut Vec<String>) {
        let er = &self.embedded_relay;
        let (true, Some(ef)) = (er.enabled, &er.secret_file) else {
            return;
        };
        let fix = format!(
            "every JOIN would be refused (JOIN_NAK 3) and every match sent there would fail. Name the same file: \
             secret_file = \"{}\", or leave secret_file out of that entry (it is then taken from [embedded_relay])",
            ef.display()
        );
        for r in self.relays.iter().filter(|r| self.names_embedded(r)) {
            let what = format!(
                "relays.{}: names the relay this service runs ([embedded_relay], port {} on this machine), whose \
                 secret is {}",
                r.key(),
                er.port,
                ef.display()
            );
            match (&r.secret_file, &r.secret_env) {
                (Some(f), _) if same_file(f, ef) => {}
                (Some(f), _) => e.push(format!(
                    "{what}; this entry names another file ({}): {fix}",
                    f.display()
                )),
                (None, Some(v)) => e.push(format!("{what}; this entry reads secret_env {v} instead: {fix}")),
                (None, None) => e.push(format!("{what}; this entry has no secret: {fix}")),
            }
        }
        if !self
            .relays
            .iter()
            .any(|r| r.secret_file.as_ref().is_some_and(|f| same_file(f, ef)))
        {
            e.push(format!(
                "embedded_relay: it opens only the rooms sealed with {}, but no [[relays]] entry announces it with \
                 that secret: matches would be sent to it with codes it refuses (JOIN_NAK 3), or never sent to it. Add \
                 secret_file = \"{}\" to the [[relays]] entry that names this machine (its public host name, port {}), \
                 or remove secret_file from [embedded_relay] for a relay open to anyone",
                ef.display(),
                ef.display(),
                er.port
            ));
        }
    }

    /// What `check-config` and the service's start report without refusing to run.
    pub fn warnings(&self) -> Vec<String> {
        let mut w = crate::spectate::warnings(self);
        w.extend(self.competitive_warnings());
        let er = &self.embedded_relay;
        if er.enabled {
            // An entry on the embedded relay's port without a secret may be this machine under another name.
            if er.secret_file.is_some() {
                for r in &self.relays {
                    if !self.names_embedded(r) && r.port == er.port && r.secret_file.is_none() && r.secret_env.is_none()
                    {
                        w.push(format!(
                            "relays.{}: no secret, on the port of the relay this service runs: if `{}` is this \
                             machine, every JOIN will be refused (JOIN_NAK 3); give it secret_file = \"{}\"",
                            r.key(),
                            r.host,
                            er.secret_file.as_deref().unwrap_or(Path::new("")).display()
                        ));
                    }
                }
            } else if self
                .relays
                .iter()
                .any(|r| self.names_embedded(r) && r.secret_file.is_some())
            {
                w.push(
                    "embedded_relay has no secret_file: the relay this service runs is open to anyone who knows its \
                     address (the codes sealed for it are accepted, like any other)"
                        .into(),
                );
            }
        }
        let s = &self.server;
        if let Some(p) = self.auth.password.as_ref().filter(|p| p.enabled) {
            if !s.production || !s.api_url.starts_with("https://") {
                w.push(
                    "auth.password is on without production = true and an https api_url: passwords must only travel \
                     over HTTPS, use this for tests only"
                        .into(),
                );
            }
            if !crate::auth::password::argon2_strong(&p.argon2) {
                w.push("auth.password.argon2 is below the OWASP minimum (allowed off production only)".into());
            }
            if p.smtp.is_none() {
                w.push(
                    "auth.password without [auth.password.smtp]: password resets need a code from you \
                     (`admin password-reset <user>`), and addresses are not verified"
                        .into(),
                );
            } else if p.smtp.as_ref().is_some_and(|m| !m.verify_signup) {
                w.push(
                    "auth.password.smtp.verify_signup = false: anyone can sign up with someone else's address".into(),
                );
            }
        }
        if s.production
            && self
                .auth
                .discord
                .as_ref()
                .is_some_and(|d| d.enabled && d.client_secret.is_some())
        {
            w.push("auth.discord.client_secret is written in the configuration: prefer client_secret_file".into());
        }
        w
    }

    /// Combinations that let one player decide competitive results with second accounts (security review M3, M7;
    /// `docs/security-model.md`): rated queues and tournaments open to guest accounts, a policy whose witnesses may be
    /// any spectator on matches whose places are open to non-referees, a rated queue whose policy rates one team's
    /// reports. Allowed (a server's choice), never silent.
    pub fn competitive_warnings(&self) -> Vec<String> {
        use kalamar_protocol::policy::{Quorum, WitnessFrom, WitnessUse};
        let mut w = Vec::new();
        // (kind, id, rated, policy, places for non-referees)
        let mut competitive: Vec<(&str, &str, bool, &str, Option<&str>)> = Vec::new();
        for q in self.queues.iter().filter(|q| q.rated) {
            let who = q.spectators.as_ref().map(|s| s.who.as_str());
            competitive.push(("queues", &q.id, true, &q.result_policy, who));
        }
        for a in self.activities.iter().filter(|a| a.kind == "tournament") {
            let who = a.spectators.as_ref().map(|s| s.who.as_str());
            competitive.push(("activities", &a.id, false, &a.result_policy, who));
        }
        if self.auth.guest.enabled && self.auth.guest.ranked && !competitive.is_empty() {
            let names: Vec<String> = competitive.iter().map(|c| format!("{}.{}", c.0, c.1)).collect();
            w.push(format!(
                "auth.guest: guest accounts may play the rated queues and tournaments ({}): a guest account costs one \
                 request, so a banned or cooled-down player is back at once and anyone can bring a second account. \
                 Set [auth.guest] ranked = false (guests keep rooms and unrated queues) and offer an OAuth login",
                names.join(", ")
            ));
        }
        for (kind, id, rated, policy, who) in &competitive {
            let Some(p) = self.policy(policy) else { continue };
            let open_stands = who.is_some_and(|w| w != "referees") && self.features.spectators;
            if p.witness != WitnessUse::Ignore && p.witness_from == WitnessFrom::Any && open_stands {
                w.push(format!(
                    "{kind}.{id}: its result policy `{policy}` lets any spectator break a tie (witness_from = \"any\") \
                     and its places in the stands are for `{}`: a player's second account in the stands decides a \
                     disagreement for that player (counted, never rated; a counted game still counts in \
                     a series or a tournament). Use witness_from = \"referees\", or places for referees only",
                    who.unwrap_or_default()
                ));
            }
            if *rated && !p.rated_requires_both_teams {
                w.push(format!(
                    "{kind}.{id}: rated, with the result policy `{policy}` whose rated_requires_both_teams = false: one \
                     team's reports alone change ratings (one player in a duel)"
                ));
            }
            if !rated && p.quorum == Quorum::Any {
                w.push(format!(
                    "{kind}.{id}: its result policy `{policy}` has quorum = \"any\": one player's report alone decides \
                     a game when the other player sends nothing"
                ));
            }
        }
        w
    }

    /// Reads every secret the configuration names (relay secrets, OAuth client secrets, the SMTP password), as the
    /// service does at start (`serve` stops on the first problem); never prints them.
    pub fn check_secrets(&self) -> Vec<String> {
        let mut e = Vec::new();
        let er = &self.embedded_relay;
        // The embedded relay's secret is created at the first start: missing is fine, unreadable is not.
        let embedded_file = er.secret_file.as_deref().filter(|_| er.enabled);
        if let Some(f) = embedded_file.filter(|f| f.exists())
            && let Err(m) = kalamar_relay::roomauth::RoomSecret::from_file(f)
        {
            e.push(format!("embedded_relay.secret_file: {m}"));
        }
        for r in &self.relays {
            if let (Some(f), Some(ef)) = (&r.secret_file, embedded_file)
                && same_file(f, ef)
            {
                continue;
            }
            if let Err(m) = crate::relays::read_relay_secret(r) {
                e.push(m);
            }
        }
        for r in crate::auth::oauth::configured(self).into_iter().flatten() {
            let what = format!("{}: client secret", r.section);
            let got = match r.secret.first() {
                Some(crate::auth::oauth::SecretSource::Inline(v)) if !v.is_empty() => Ok(String::new()),
                Some(crate::auth::oauth::SecretSource::Inline(_)) | None => Err(format!("{what}: missing")),
                Some(crate::auth::oauth::SecretSource::File(f)) => read_secret(Some(f), None, &what),
                Some(crate::auth::oauth::SecretSource::Env(n)) => read_secret(None, Some(n), &what),
            };
            if let Err(m) = got {
                e.push(m);
            }
        }
        if let Some(s) = self
            .auth
            .password
            .as_ref()
            .filter(|p| p.enabled)
            .and_then(|p| p.smtp.as_ref())
            .filter(|s| s.username.is_some())
            && let Err(m) = read_secret(
                s.password_file.as_deref(),
                s.password_env.as_deref(),
                "auth.password.smtp password",
            )
        {
            e.push(m);
        }
        // Security review (information): a webhook whose secret file could not be read was dropped silently.
        for w in self.webhooks.iter().filter(|w| w.secret.is_none()) {
            if let Some(f) = &w.secret_file
                && let Err(m) = read_secret(Some(f), None, &format!("webhooks `{}` secret", w.url))
            {
                e.push(m);
            }
        }
        e
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = r#"
[server]
name = "T"
api_url = "https://play.example.org/api"
privacy_url = "https://example.invalid/privacy"
operator = { name = "T", contact = "mailto:t@example.invalid" }
[features]
rooms = true
queues = false
parties = false
ratings = false
leaderboards = false
[auth.guest]
enabled = true
"#;

    fn parse(extra: &str) -> Result<Config, String> {
        Config::parse(&format!("{BASE}{extra}"))
    }

    const BOUND: &str = "[embedded_relay]\nenabled = true\nport = 47313\nsecret_file = \"data/relay.key\"\n";

    /// A webhook whose secret file cannot be read is reported by check-config (never left out silently).
    #[test]
    fn an_unreadable_webhook_secret_is_reported() {
        let c = parse(concat!(
            "[[relays]]\nhost = \"play.example.org\"\n",
            "[[webhooks]]\nurl = \"https://bot.example.org/k\"\nsecret_file = \"no/such/webhook-secret\"\n",
        ))
        .unwrap();
        let e = c.check_secrets().join("\n");
        assert!(e.contains("webhooks `https://bot.example.org/k` secret"), "{e}");
    }

    /// A 1.1.7 / 1.1.8 file (one [[relays]] without id or secret, no [embedded_relay]) means what it meant.
    #[test]
    fn a_configuration_of_1_1_8_is_unchanged() {
        let c = parse("[[relays]]\nhost = \"play.example.org\"\nport = 47313\n").unwrap();
        assert!(!c.embedded_relay.enabled);
        assert!(c.relays[0].secret_file.is_none() && c.relays[0].secret_env.is_none());
        assert!(c.derived.is_empty() && c.warnings().is_empty() && c.check_secrets().is_empty());
        assert_eq!(c.relays[0].key(), "play.example.org:47313");
    }

    /// The entry that names the bound embedded relay gets its secret: written once, in [embedded_relay].
    #[test]
    fn the_entry_naming_the_embedded_relay_gets_its_secret() {
        let other = "[[relays]]\nhost = \"relay-us.example.org\"\nid = \"us\"\nsecret_file = \"us.key\"\n";
        for here in [
            "[[relays]]\nhost = \"relay.example.org\"\n", // the only entry, under any name
            "[[relays]]\nhost = \"play.example.org\"\n",  // the host of api_url
            "[[relays]]\nhost = \"PLAY.example.org.\"\n", // the same, written otherwise
            "[[relays]]\nhost = \"127.0.0.1\"\n",         // loopback
            "[[relays]]\nhost = \"localhost\"\n",
        ] {
            let several = here != "[[relays]]\nhost = \"relay.example.org\"\n";
            let text = format!("{BOUND}{here}{}", if several { other } else { "" });
            let c = parse(&text).unwrap_or_else(|e| panic!("{here}: {e}"));
            assert_eq!(c.relays[0].secret_file, Some(PathBuf::from("data/relay.key")), "{here}");
            assert_eq!(c.derived.len(), 1, "{here}");
            if several {
                assert_eq!(c.relays[1].secret_file, Some(PathBuf::from("us.key")), "{here}");
            }
        }
        // Normalizing twice (start, then reload) changes nothing more.
        let mut c = parse(&format!("{BOUND}[[relays]]\nhost = \"play.example.org\"\n")).unwrap();
        c.normalize();
        assert_eq!(c.derived.len(), 1);
        // An open embedded relay gives nothing.
        let c = parse("[embedded_relay]\nenabled = true\n[[relays]]\nhost = \"play.example.org\"\n").unwrap();
        assert!(c.relays[0].secret_file.is_none() && c.derived.is_empty());
    }

    /// The trap of a bound embedded relay announced without its secret: every JOIN refused, every match failed.
    #[test]
    fn a_bound_embedded_relay_must_be_announced_with_its_secret() {
        // The entry that names it holds another secret.
        let e = parse(&format!(
            "{BOUND}[[relays]]\nhost = \"play.example.org\"\nsecret_file = \"other.key\"\n"
        ))
        .unwrap_err();
        assert!(
            e.contains("relays.play.example.org:47313: names the relay this service runs")
                && e.contains("other.key")
                && e.contains("JOIN_NAK 3"),
            "{e}"
        );
        // ... or a variable instead of the file.
        let e = parse(&format!(
            "{BOUND}[[relays]]\nhost = \"127.0.0.1\"\nsecret_env = \"X\"\n[[relays]]\nhost = \"b.example.org\"\n"
        ))
        .unwrap_err();
        assert!(e.contains("secret_env X"), "{e}");
        // No entry names this machine, none has its secret.
        let e = parse(&format!(
            "{BOUND}[[relays]]\nhost = \"relay.example.org\"\nid = \"a\"\n[[relays]]\nhost = \"relay-us.example.org\"\nid = \"b\"\n"
        ))
        .unwrap_err();
        assert!(
            e.contains("embedded_relay: it opens only the rooms sealed with data/relay.key"),
            "{e}"
        );
        // The same file written otherwise is the same file.
        parse(&format!(
            "{BOUND}[[relays]]\nhost = \"play.example.org\"\nsecret_file = \"./data/relay.key\"\n[[relays]]\nhost = \"b.example.org\"\n"
        ))
        .unwrap();
        parse(&format!(
            "{BOUND}[[relays]]\nhost = \"relay.example.org\"\nid = \"a\"\nsecret_file = \"data/x/../relay.key\"\n[[relays]]\nhost = \"b.example.org\"\n"
        ))
        .unwrap();
    }

    #[test]
    fn paths_are_compared_normalized() {
        assert!(same_file(Path::new("data/relay.key"), Path::new("./data/relay.key")));
        assert!(same_file(Path::new("data/x/../relay.key"), Path::new("data/relay.key")));
        let abs = std::env::current_dir().unwrap().join("data").join("relay.key");
        assert!(same_file(&abs, Path::new("data/relay.key")));
        assert!(!same_file(Path::new("data/relay.key"), Path::new("data/relay2.key")));
        assert!(!same_file(Path::new("data/relay.key"), Path::new("other/relay.key")));
    }

    /// An entry on the embedded relay's port without a secret may be this machine under another name: said, not
    /// refused (it may as well be someone else's open relay).
    #[test]
    fn an_entry_that_may_be_this_machine_without_its_secret_is_warned_about() {
        let c = parse(&format!(
            "{BOUND}[[relays]]\nhost = \"play.example.org\"\n[[relays]]\nhost = \"relay2.example.org\"\nid = \"r2\"\n"
        ))
        .unwrap();
        let w = c.warnings().join("\n");
        assert!(
            w.contains("relays.r2: no secret, on the port of the relay this service runs"),
            "{w}"
        );
        // Another port: nothing to say.
        let c = parse(&format!(
            "{BOUND}[[relays]]\nhost = \"play.example.org\"\n[[relays]]\nhost = \"relay2.example.org\"\nport = 47400\n"
        ))
        .unwrap();
        assert!(c.warnings().is_empty(), "{:?}", c.warnings());
    }

    /// A reload while [embedded_relay] waits for a restart keeps the running [[relays]] too.
    #[test]
    fn relays_wait_with_the_embedded_relay() {
        let running = parse("[[relays]]\nhost = \"play.example.org\"\n").unwrap();
        let mut new = parse(
            "[embedded_relay]\nenabled = true\nport = 47400\n[[relays]]\nhost = \"play.example.org\"\nport = 47400\n",
        )
        .unwrap();
        assert_eq!(
            new.keep_restart_only(&running),
            vec!["embedded_relay", "relays (they wait with embedded_relay)"]
        );
        assert_eq!((new.relays[0].port, new.embedded_relay.enabled), (47313, false));
        assert!(new.validate().is_empty());
        // Without a change of [embedded_relay], a change of [[relays]] applies at once.
        let mut new = parse("[[relays]]\nhost = \"play.example.org\"\n[[relays]]\nhost = \"b.example.org\"\n").unwrap();
        assert!(new.keep_restart_only(&running).is_empty());
        assert_eq!(new.relays.len(), 2);
    }

    #[test]
    fn the_embedded_relays_limits_are_checked() {
        // 0 is no cap (1.1.13)
        let c = parse("[embedded_relay]\nenabled = true\nmax_rooms = 0\nmax_sessions = 0\n[[relays]]\nhost = \"a\"\n")
            .unwrap();
        let r = c.embedded_relay.relay_config(None);
        assert_eq!((r.max_rooms, r.max_sessions), (0, 0));
        let e = parse("[embedded_relay]\nenabled = true\nworkers = 1000\n[[relays]]\nhost = \"a\"\n").unwrap_err();
        assert!(e.contains("embedded_relay.workers"), "{e}");
        let c = parse("[embedded_relay]\nenabled = true\n[[relays]]\nhost = \"a\"\n").unwrap();
        let n = kalamar_relay::server::auto_workers();
        assert_eq!(
            c.embedded_relay.worker_count(),
            n.div_ceil(2),
            "half the processors by default"
        );
        let c = parse("[embedded_relay]\nenabled = true\nworkers = 3\n[[relays]]\nhost = \"a\"\n").unwrap();
        assert_eq!(c.embedded_relay.worker_count(), 3);
        let c =
            parse("[embedded_relay]\nenabled = true\nmax_rooms = 16\nmax_sessions = 64\n[[relays]]\nhost = \"a\"\n")
                .unwrap();
        let r = c.embedded_relay.relay_config(None);
        assert_eq!((r.max_rooms, r.max_sessions), (16, 64));
        // The per-source limits: kalamar-relay's defaults, raised for players behind one address (a LAN party).
        let d = kalamar_relay::relay::RelayConfig::default();
        assert_eq!(
            (r.max_sessions_per_source, r.max_members_per_source),
            (d.max_sessions_per_source, 8)
        );
        let c = parse(
            "[embedded_relay]\nenabled = true\nmax_members_per_source = 12\nmax_sessions_per_source = 0\n[[relays]]\nhost = \"a\"\n",
        )
        .unwrap();
        let r = c.embedded_relay.relay_config(None);
        assert_eq!((r.max_sessions_per_source, r.max_members_per_source), (0, 12));
    }

    #[test]
    fn the_api_host_is_the_host_of_api_url() {
        let mut c = parse("[[relays]]\nhost = \"a\"\n").unwrap();
        assert_eq!(c.api_host(), "play.example.org");
        c.server.api_url = "https://Play.Example.org:8443/api".into();
        assert_eq!(c.api_host(), "play.example.org");
        c.server.api_url = "http://[::1]:8080/api".into();
        assert_eq!(c.api_host(), "::1");
    }
}
