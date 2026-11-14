-- E-mail and password accounts (auth.password). The address itself is never stored: the account is found through
-- `identities` (provider 'password', subject_hash = HMAC-SHA-256(server secret, 'password' || 0 || normalised
-- address)), like every other login identity.

CREATE TABLE password_logins (
    user_id TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    phc TEXT NOT NULL,             -- Argon2id PHC string: algorithm, parameters, per-account random salt, hash
    changed_at INTEGER NOT NULL
);

-- One-time codes (reset, sign-up verification): only a keyed hash of the code, single use, expiring.
CREATE TABLE password_codes (
    subject_hash BLOB NOT NULL,    -- the keyed hash of the address the code is for
    purpose TEXT NOT NULL,         -- reset | signup
    user_id TEXT REFERENCES users(id) ON DELETE CASCADE,   -- reset codes: the account
    code_hash BLOB NOT NULL,       -- HMAC-SHA-256(server secret, purpose, subject_hash, code)
    origin TEXT NOT NULL,          -- email | operator
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (subject_hash, purpose)
);
CREATE INDEX password_codes_user ON password_codes(user_id);
CREATE INDEX password_codes_expiry ON password_codes(expires_at);
