-- Members, entitlements, invitations and password tokens (spec 0002 slice 5,
-- ADR 0006, domain model §1–2). Timestamps are UTC ISO-8601 text with
-- millisecond precision.

-- Entitlement: member × category (× optional service instance, unused until
-- service roles exist). Personal: owned by `member_id`.
CREATE TABLE entitlement (
    id                  INTEGER PRIMARY KEY,
    member_id           INTEGER NOT NULL REFERENCES member (id) ON DELETE CASCADE,
    category            TEXT NOT NULL CHECK (category IN ('movies', 'series', 'music', 'adult')),
    -- NULL = the whole category. Prepared for per-instance entitlements; no
    -- foreign key yet because `service_instance` arrives in another slice.
    service_instance_id INTEGER,
    enabled             INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0, 1)),
    request_mode        TEXT NOT NULL DEFAULT 'approval' CHECK (request_mode IN ('auto', 'approval')),
    updated_at          TEXT NOT NULL
) STRICT;

-- One entitlement per member, category and instance (NULL instance counted
-- as one value). Includes the owner (lesson 4).
CREATE UNIQUE INDEX entitlement_member_category
    ON entitlement (member_id, category, ifnull(service_instance_id, 0));

-- Deletion order for a member that cannot be deleted outright (domain model
-- §1): the member is `retired` until `grace_until`, can be restored until
-- then, and becomes a tombstone afterwards (`tombstoned_at`). The row stays
-- with the tombstone: who requested the deletion and when.
CREATE TABLE member_deletion (
    member_id     INTEGER PRIMARY KEY REFERENCES member (id) ON DELETE CASCADE,
    -- Admin username as text: survives removal of the admin.
    requested_by  TEXT NOT NULL,
    requested_at  TEXT NOT NULL,
    grace_until   TEXT NOT NULL,
    tombstoned_at TEXT
) STRICT;

CREATE INDEX member_deletion_due ON member_deletion (grace_until) WHERE tombstoned_at IS NULL;

-- Invitation links (ADR 0006). Only the SHA-256 hash of the token is stored,
-- so the link can be shown once, at creation.
CREATE TABLE invitation (
    id                  INTEGER PRIMARY KEY,
    token_hash          BLOB NOT NULL UNIQUE,
    created_by          INTEGER REFERENCES admin_account (id) ON DELETE SET NULL,
    created_at          TEXT NOT NULL,
    expires_at          TEXT NOT NULL,
    used_at             TEXT,
    used_by_member_id   INTEGER REFERENCES member (id) ON DELETE SET NULL,
    revoked_at          TEXT,
    preset_username     TEXT COLLATE NOCASE,
    preset_email        TEXT COLLATE NOCASE,
    -- JSON array of entitlements ({category, enabled, requestMode}).
    preset_entitlements TEXT NOT NULL DEFAULT '[]'
) STRICT;

CREATE INDEX invitation_created_at ON invitation (created_at);
CREATE INDEX invitation_created_by ON invitation (created_by);
CREATE INDEX invitation_used_by_member_id ON invitation (used_by_member_id);

-- One-time set-password / reset links. The subject is a member (admins
-- later); no FK because it may point at one of two tables, like `session`.
CREATE TABLE password_token (
    id           INTEGER PRIMARY KEY,
    token_hash   BLOB NOT NULL UNIQUE,
    subject_kind TEXT NOT NULL CHECK (subject_kind IN ('admin', 'member')),
    subject_id   INTEGER NOT NULL,
    purpose      TEXT NOT NULL CHECK (purpose IN ('set', 'reset')),
    created_at   TEXT NOT NULL,
    expires_at   TEXT NOT NULL,
    used_at      TEXT
) STRICT;

CREATE INDEX password_token_subject ON password_token (subject_kind, subject_id);

CREATE INDEX member_status ON member (status);
CREATE INDEX audit_log_actor ON audit_log (actor_kind, actor_name);
