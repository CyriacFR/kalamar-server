-- Protocol 1.1: series of matches. The whole series is kept as JSON (sides, games, wins); matches name their series
-- inside their `game` JSON.

CREATE TABLE series (
    id TEXT PRIMARY KEY,
    origin_kind TEXT NOT NULL,
    origin_id TEXT NOT NULL,
    state TEXT NOT NULL,
    data TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    ended_at INTEGER
);
CREATE INDEX series_state ON series(state);
