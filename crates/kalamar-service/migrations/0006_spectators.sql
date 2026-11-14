-- Protocol 1.1.7: places in the stands of a match (spectators.md). A spectator is never a player: its seat, statuses,
-- witness report and verdict live apart from match_players, so ratings, statistics and history stay the players'.
CREATE TABLE match_spectators (
    match_id TEXT NOT NULL REFERENCES matches(id) ON DELETE CASCADE,
    slot INTEGER NOT NULL,
    user_id TEXT REFERENCES users(id) ON DELETE SET NULL,
    display_name TEXT NOT NULL,
    member_name TEXT NOT NULL,
    role TEXT NOT NULL,              -- host or join: its part in the game session
    referee INTEGER NOT NULL DEFAULT 0,
    launched_at INTEGER,
    status TEXT,                     -- the last mirrored status file (JSON)
    status_state TEXT,
    status_at INTEGER,
    report TEXT,                     -- the witness report (JSON), purged with the players' after the retention
    report_at INTEGER,
    left_place INTEGER NOT NULL DEFAULT 0,
    verdict TEXT,                    -- agreed, dissent, reported, invalid, error, silent, absent
    PRIMARY KEY (match_id, slot)
);
CREATE INDEX match_spectators_user ON match_spectators(user_id);

-- A player may refuse to be watched when the operator lets them ([spectators] opt_out).
ALTER TABLE users ADD COLUMN allow_spectators INTEGER NOT NULL DEFAULT 1;
