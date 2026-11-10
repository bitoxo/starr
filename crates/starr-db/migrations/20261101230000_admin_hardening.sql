-- Admin hardening (spec 0002 slice 6, ADR 0005): TOTP with recovery codes,
-- two-step login, admin accounts, linked member + switch. Timestamps are UTC
-- ISO-8601 text with millisecond precision.

-- TOTP enrollment in progress: the sealed secret waits here until the admin
-- confirms it with a code; `totp_secret_sealed` (initial migration) then holds
-- it and TOTP is on.
ALTER TABLE admin_account ADD COLUMN totp_pending_sealed BLOB;
-- Last accepted TOTP time step: a code is accepted once per step (RFC 6238
-- §5.2), so a code seen by an attacker cannot be replayed.
ALTER TABLE admin_account ADD COLUMN totp_last_step INTEGER;

-- An admin in member view: the session's subject is the linked member, and
-- this is the admin who may switch back (after re-authenticating).
ALTER TABLE session ADD COLUMN origin_admin_id INTEGER REFERENCES admin_account (id) ON DELETE CASCADE;
CREATE INDEX session_origin_admin_id ON session (origin_admin_id) WHERE origin_admin_id IS NOT NULL;

-- One-time recovery codes for the second factor. Only SHA-256 hashes are
-- stored (codes carry 60 random bits); each works once.
CREATE TABLE admin_recovery_code (
    id         INTEGER PRIMARY KEY,
    admin_id   INTEGER NOT NULL REFERENCES admin_account (id) ON DELETE CASCADE,
    code_hash  BLOB NOT NULL,
    created_at TEXT NOT NULL,
    used_at    TEXT
) STRICT;

CREATE UNIQUE INDEX admin_recovery_code_admin_hash ON admin_recovery_code (admin_id, code_hash);

-- Between the password step and the TOTP step of a login. Not a session:
-- it authorizes nothing but the second step, once, for a few minutes. Only
-- the SHA-256 hash of the token is stored.
CREATE TABLE pending_login (
    token_hash BLOB PRIMARY KEY NOT NULL,
    admin_id   INTEGER NOT NULL REFERENCES admin_account (id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
) STRICT;

CREATE INDEX pending_login_admin_id ON pending_login (admin_id);
CREATE INDEX pending_login_expires_at ON pending_login (expires_at);

-- One active admin always remains (domain model §1). The API also requires
-- an active actor other than the target; this trigger is the last line, and
-- it covers every writer (API, recovery CLI, manual SQL).
CREATE TRIGGER admin_account_keep_one_active
BEFORE UPDATE OF suspended_at, removed_at ON admin_account
WHEN OLD.suspended_at IS NULL AND OLD.removed_at IS NULL
 AND (NEW.suspended_at IS NOT NULL OR NEW.removed_at IS NOT NULL)
 AND NOT EXISTS (
     SELECT 1 FROM admin_account
     WHERE id <> OLD.id AND suspended_at IS NULL AND removed_at IS NULL
 )
BEGIN
    SELECT RAISE(ABORT, 'last_active_admin');
END;

-- Admin rows are never deleted (tombstones keep the username), but if one
-- is, the same rule holds.
CREATE TRIGGER admin_account_keep_one_active_delete
BEFORE DELETE ON admin_account
WHEN OLD.suspended_at IS NULL AND OLD.removed_at IS NULL
 AND NOT EXISTS (
     SELECT 1 FROM admin_account
     WHERE id <> OLD.id AND suspended_at IS NULL AND removed_at IS NULL
 )
BEGIN
    SELECT RAISE(ABORT, 'last_active_admin');
END;
