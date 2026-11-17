-- The reference service's choice of a match's host (`src/host.rs`): a hidden score of how the player's game hosted its
-- last matches, 100 for everyone at first, from 0 to 200. Erased with the account (the row goes).
ALTER TABLE users ADD COLUMN host_score INTEGER NOT NULL DEFAULT 100;
