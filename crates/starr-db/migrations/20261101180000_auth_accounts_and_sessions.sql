-- Auth core (ADR 0005, spec 0002 slice 3): admin and member identities,
-- sessions and login attempts. Timestamps are UTC ISO-8601 text with
-- millisecond precision (fixed width, so text order is time order).

-- Members: table only in slice 3; CRUD arrives with slice 5.
CREATE TABLE member (
    id            INTEGER PRIMARY KEY,
    username      TEXT NOT NULL UNIQUE COLLATE NOCASE,
    email         TEXT UNIQUE COLLATE NOCASE,
    display_name  TEXT NOT NULL,
    -- NULL until the member sets a password via a set-password link.
    password_hash TEXT,
    status        TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'suspended', 'retired')),
    language      TEXT NOT NULL DEFAULT 'en',
    created_at    TEXT NOT NULL
) STRICT;

CREATE TABLE admin_account (
    id                 INTEGER PRIMARY KEY,
    username           TEXT NOT NULL UNIQUE COLLATE NOCASE,
    display_name       TEXT NOT NULL,
    password_hash      TEXT NOT NULL,
    totp_secret_sealed BLOB,
    linked_member_id   INTEGER REFERENCES member (id) ON DELETE SET NULL,
    suspended_at       TEXT,
    -- Tombstone: removed admins keep their row (and username) for the audit trail.
    removed_at         TEXT,
    created_by         INTEGER REFERENCES admin_account (id) ON DELETE SET NULL,
    language           TEXT NOT NULL DEFAULT 'en',
    created_at         TEXT NOT NULL
) STRICT;

-- A member profile is linked to at most one admin account.
CREATE UNIQUE INDEX admin_account_linked_member ON admin_account (linked_member_id)
    WHERE linked_member_id IS NOT NULL;
CREATE INDEX admin_account_created_by ON admin_account (created_by);

-- Usernames are unique across admins and members (case-insensitive).
CREATE TRIGGER admin_account_username_unique_insert
BEFORE INSERT ON admin_account
WHEN EXISTS (SELECT 1 FROM member WHERE username = NEW.username COLLATE NOCASE)
BEGIN
    SELECT RAISE(ABORT, 'username_taken');
END;

CREATE TRIGGER admin_account_username_unique_update
BEFORE UPDATE OF username ON admin_account
WHEN EXISTS (SELECT 1 FROM member WHERE username = NEW.username COLLATE NOCASE)
BEGIN
    SELECT RAISE(ABORT, 'username_taken');
END;

CREATE TRIGGER member_username_unique_insert
BEFORE INSERT ON member
WHEN EXISTS (SELECT 1 FROM admin_account WHERE username = NEW.username COLLATE NOCASE)
BEGIN
    SELECT RAISE(ABORT, 'username_taken');
END;

CREATE TRIGGER member_username_unique_update
BEFORE UPDATE OF username ON member
WHEN EXISTS (SELECT 1 FROM admin_account WHERE username = NEW.username COLLATE NOCASE)
BEGIN
    SELECT RAISE(ABORT, 'username_taken');
END;

-- Only the SHA-256 hash of the session token is stored. The subject is either
-- an admin_account or a member row (no FK: it points at one of two tables).
CREATE TABLE session (
    token_hash     BLOB PRIMARY KEY NOT NULL,
    subject_kind   TEXT NOT NULL CHECK (subject_kind IN ('admin', 'member')),
    subject_id     INTEGER NOT NULL,
    created_at     TEXT NOT NULL,
    last_seen_at   TEXT NOT NULL,
    expires_at     TEXT NOT NULL,
    ip             TEXT,
    user_agent     TEXT,
    -- Re-authentication window for admin switch-back (slice 6).
    elevated_until TEXT
) STRICT;

CREATE INDEX session_subject ON session (subject_kind, subject_id);
CREATE INDEX session_expires_at ON session (expires_at);
CREATE INDEX session_last_seen_at ON session (last_seen_at);

-- Login throttling lives in the database so every process sees it.
CREATE TABLE login_attempt (
    id            INTEGER PRIMARY KEY,
    -- Hash of the normalized username as submitted (the account may not
    -- exist, and people sometimes type their password into this field).
    username_hash TEXT NOT NULL,
    -- Client IP, IPv6 grouped by /64.
    ip            TEXT NOT NULL,
    -- Device cookie id when the attempt presented a valid one for this
    -- username; such attempts do not count toward the username limit.
    device        TEXT,
    at            TEXT NOT NULL,
    success       INTEGER NOT NULL CHECK (success IN (0, 1))
) STRICT;

CREATE INDEX login_attempt_username_at ON login_attempt (username_hash, at);
CREATE INDEX login_attempt_ip_at ON login_attempt (ip, at);
CREATE INDEX login_attempt_device_at ON login_attempt (device, at);
CREATE INDEX login_attempt_at ON login_attempt (at);
