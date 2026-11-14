-- Requests the admin command line leaves for the running service (`admin reload`): the service's ticker takes them
-- within two seconds and writes back what happened.
CREATE TABLE control (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL,              -- reload
    requested_at INTEGER NOT NULL,
    done_at INTEGER,
    ok INTEGER,
    detail TEXT                      -- what the service answered (the report, or the errors)
);
