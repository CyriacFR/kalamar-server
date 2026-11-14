-- Protocol 1.1 features of the reference service: activities survive a restart, badges and titles.

CREATE TABLE activity_state (
    id TEXT PRIMARY KEY,
    data TEXT NOT NULL,            -- JSON of the activity's live state
    updated_at INTEGER NOT NULL
);

CREATE TABLE user_badges (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    badge_id TEXT NOT NULL,
    earned_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, badge_id)
);

ALTER TABLE users ADD COLUMN featured_badge TEXT;
ALTER TABLE users ADD COLUMN title TEXT;
