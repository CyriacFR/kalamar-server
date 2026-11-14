-- Protocol 1.1.12, security review of 2026-11-13.
--
-- M6: what an erased account's login identity still answers for, for a limited time (PRIVACY-TEMPLATE.md, section 5):
-- the queue cooldown that was running, the offences of the penalty window, the open reports about it. Keyed like
-- `identities` (a keyed hash, never the provider's user id or an address); a new account with the same identity takes
-- them over; the row is deleted at `until` (and earlier when nothing is left to hold). Bans keep `identity_bans`.
CREATE TABLE identity_holds (
    provider TEXT NOT NULL,
    subject_hash BLOB NOT NULL,
    hold_id TEXT NOT NULL,           -- shared by every identity of the erased account (the reports below name it)
    until INTEGER NOT NULL,
    cooldown_until INTEGER,
    offences TEXT NOT NULL DEFAULT '[]', -- JSON [[kind, at, cooldown_until], ...]
    created_at INTEGER NOT NULL,
    PRIMARY KEY (provider, subject_hash)
);
CREATE INDEX identity_holds_id ON identity_holds(hold_id);

-- Reports about an account are no longer deleted with it: their target becomes NULL and, while they are open and a hold
-- runs, `target_hold` names it. A resolved report goes with the retention of closed reports, as before.
CREATE TABLE reports_v2 (
    id TEXT PRIMARY KEY,
    reporter TEXT REFERENCES users(id) ON DELETE SET NULL,
    target TEXT REFERENCES users(id) ON DELETE SET NULL,
    match_id TEXT,
    category TEXT NOT NULL,
    comment TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'open',
    resolution TEXT,
    resolved_at INTEGER,
    target_hold TEXT
);
INSERT INTO reports_v2 (id, reporter, target, match_id, category, comment, created_at, status, resolution, resolved_at)
    SELECT id, reporter, target, match_id, category, comment, created_at, status, resolution, resolved_at FROM reports;
DROP TABLE reports;
ALTER TABLE reports_v2 RENAME TO reports;
CREATE INDEX reports_status ON reports(status, created_at);
CREATE INDEX reports_hold ON reports(target_hold);

-- M1 (server side): the game declaration's `build` is no longer stored (launchers sent a per-install identifier).
UPDATE users SET game_caps = json_remove(game_caps, '$.build')
    WHERE game_caps IS NOT NULL AND json_valid(game_caps);
