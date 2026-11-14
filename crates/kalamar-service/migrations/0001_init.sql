-- Kalamar service schema, version 1. Times are UTC Unix seconds. Tokens and login identities are stored hashed.

CREATE TABLE users (
    id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    name_changed_at INTEGER,
    friend_code TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    share_presence INTEGER NOT NULL DEFAULT 1,
    public_profile INTEGER NOT NULL DEFAULT 1,
    show_on_leaderboards INTEGER NOT NULL DEFAULT 1,
    allow_friend_requests INTEGER NOT NULL DEFAULT 1,
    banned_until INTEGER,          -- NULL: not banned; 9223372036854775807: permanent
    ban_reason TEXT,
    queue_cooldown_until INTEGER,
    game_caps TEXT                 -- JSON of PUT /v1/me/game
);

CREATE TABLE identities (
    provider TEXT NOT NULL,
    subject_hash BLOB NOT NULL,    -- HMAC-SHA-256(server secret, provider || 0 || provider user id)
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (provider, subject_hash)
);
CREATE INDEX identities_user ON identities(user_id);

-- Kept while a ban of an erased account runs, to stop ban evasion.
CREATE TABLE identity_bans (
    provider TEXT NOT NULL,
    subject_hash BLOB NOT NULL,
    until INTEGER NOT NULL,
    reason TEXT,
    PRIMARY KEY (provider, subject_hash)
);

CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    access_hash BLOB NOT NULL UNIQUE,
    access_expires INTEGER NOT NULL,
    refresh_hash BLOB NOT NULL UNIQUE,
    refresh_expires INTEGER NOT NULL,
    prev_refresh_hash BLOB,
    prev_refresh_until INTEGER,
    created_at INTEGER NOT NULL,
    last_used_at INTEGER NOT NULL
);
CREATE INDEX sessions_user ON sessions(user_id);
CREATE INDEX sessions_prev ON sessions(prev_refresh_hash);

CREATE TABLE friendships (
    user_a TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    user_b TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (user_a, user_b),
    CHECK (user_a < user_b)
);
CREATE INDEX friendships_b ON friendships(user_b);

CREATE TABLE friend_requests (
    id TEXT PRIMARY KEY,
    from_user TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    to_user TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    UNIQUE (from_user, to_user)
);
CREATE INDEX friend_requests_to ON friend_requests(to_user);

CREATE TABLE blocks (
    blocker TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    blocked TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (blocker, blocked)
);
CREATE INDEX blocks_blocked ON blocks(blocked);

CREATE TABLE seasons (
    id TEXT PRIMARY KEY,
    number INTEGER NOT NULL UNIQUE,
    name TEXT NOT NULL,
    starts_at INTEGER NOT NULL,
    ends_at INTEGER
);

CREATE TABLE matches (
    id TEXT PRIMARY KEY,
    origin_kind TEXT NOT NULL,     -- room | queue
    origin_id TEXT NOT NULL,
    state TEXT NOT NULL,
    rules TEXT NOT NULL,           -- JSON
    game TEXT NOT NULL,            -- JSON (holds the relay room: never shown to non-participants)
    season_id TEXT,
    created_at INTEGER NOT NULL,
    ready_at INTEGER,
    ended_at INTEGER,
    accept_deadline INTEGER,
    launch_deadline INTEGER,
    result_deadline INTEGER,
    first_result_at INTEGER,
    cancel_reason TEXT,
    outcome TEXT                   -- JSON
);
CREATE INDEX matches_state ON matches(state);
CREATE INDEX matches_ended ON matches(ended_at);

CREATE TABLE match_players (
    match_id TEXT NOT NULL REFERENCES matches(id) ON DELETE CASCADE,
    slot INTEGER NOT NULL,
    user_id TEXT REFERENCES users(id) ON DELETE SET NULL,
    display_name TEXT NOT NULL,
    member_name TEXT NOT NULL,
    team TEXT NOT NULL,
    role TEXT NOT NULL,
    party_id TEXT,
    ticket_id TEXT,
    accept TEXT NOT NULL,
    launched_at INTEGER,
    status TEXT,                   -- last mirrored status file (raw)
    status_state TEXT,
    status_at INTEGER,
    saw_in_match INTEGER NOT NULL DEFAULT 0,
    report TEXT,                   -- terminal report (raw)
    report_at INTEGER,
    verdict TEXT,
    result TEXT,                   -- win | loss | none
    stats TEXT,                    -- JSON: the player's line of the accepted record
    rating_delta REAL,
    PRIMARY KEY (match_id, slot)
);
CREATE INDEX match_players_user ON match_players(user_id);

CREATE TABLE ratings (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    ladder TEXT NOT NULL,
    season_id TEXT NOT NULL,
    rating REAL NOT NULL,
    deviation REAL NOT NULL,
    volatility REAL NOT NULL,
    games INTEGER NOT NULL,
    wins INTEGER NOT NULL,
    losses INTEGER NOT NULL,
    last_delta REAL,
    last_match TEXT,
    last_at INTEGER,
    PRIMARY KEY (user_id, ladder, season_id)
);
CREATE INDEX ratings_board ON ratings(ladder, season_id);

CREATE TABLE offences (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    match_id TEXT,
    at INTEGER NOT NULL,
    cooldown_until INTEGER
);
CREATE INDEX offences_user ON offences(user_id, at);

CREATE TABLE reports (
    id TEXT PRIMARY KEY,
    reporter TEXT REFERENCES users(id) ON DELETE SET NULL,
    target TEXT REFERENCES users(id) ON DELETE CASCADE,
    match_id TEXT,
    category TEXT NOT NULL,
    comment TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'open',
    resolution TEXT,
    resolved_at INTEGER
);
CREATE INDEX reports_status ON reports(status, created_at);

-- Administrative actions (who did what to which account), never personal data beyond ids.
CREATE TABLE audit (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    at INTEGER NOT NULL,
    action TEXT NOT NULL,
    target TEXT,
    detail TEXT
);
