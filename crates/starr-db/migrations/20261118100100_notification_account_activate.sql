-- Member notification "activate your account" (spec 0005 slice 5, ADR
-- 0010): an account became due on a live instance (new entitlement, new
-- instance, a password change the worker could not apply in time) and
-- Starr needs the member's password once. `account_activate` joins the
-- member kinds; the payload names the service (as `title`).
--
-- The kind lists are CHECK constraints, which SQLite cannot alter: the
-- notification table is rebuilt the same way as for `import_blocked`
-- (20261112100000): notification_delivery references it with ON DELETE
-- CASCADE and foreign keys stay on inside the migration's transaction, so
-- both tables are copied, the old ones dropped child first, and the
-- renames carry the reference over to `notification`. An older binary
-- reads an `account_activate` row as an unknown kind and skips it.

CREATE TABLE notification_new (
    id         INTEGER PRIMARY KEY,
    member_id  INTEGER REFERENCES member (id) ON DELETE CASCADE,
    kind       TEXT NOT NULL CHECK (kind IN ('request_approved', 'request_rejected',
                   'request_revoked', 'request_partially_available', 'request_available',
                   'request_failed', 'follow_requested', 'account_activate',
                   'approval_pending', 'acquisition_failed', 'import_blocked')),
    payload    TEXT NOT NULL CHECK (json_valid(payload)),
    dedup_key  TEXT NOT NULL,
    created_at TEXT NOT NULL,
    read_at    TEXT,
    -- Member kinds go to a member, admin kinds to the admin stream.
    CHECK ((member_id IS NULL)
           = (kind IN ('approval_pending', 'acquisition_failed', 'import_blocked')))
) STRICT;

INSERT INTO notification_new (id, member_id, kind, payload, dedup_key, created_at, read_at)
    SELECT id, member_id, kind, payload, dedup_key, created_at, read_at FROM notification;

CREATE TABLE notification_delivery_new (
    id              INTEGER PRIMARY KEY,
    notification_id INTEGER NOT NULL REFERENCES notification_new (id) ON DELETE CASCADE,
    channel_id      INTEGER NOT NULL REFERENCES notification_channel (id) ON DELETE CASCADE,
    state           TEXT NOT NULL CHECK (state IN ('pending', 'delivered', 'failed')),
    attempts        INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at TEXT,
    error_code      TEXT,
    last_error      TEXT,
    delivered_at    TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    UNIQUE (notification_id, channel_id),
    CHECK (state <> 'failed' OR last_error IS NOT NULL)
) STRICT;

INSERT INTO notification_delivery_new (id, notification_id, channel_id, state, attempts,
        next_attempt_at, error_code, last_error, delivered_at, created_at, updated_at)
    SELECT id, notification_id, channel_id, state, attempts, next_attempt_at, error_code,
        last_error, delivered_at, created_at, updated_at
    FROM notification_delivery;

DROP TABLE notification_delivery;
DROP TABLE notification;

ALTER TABLE notification_new RENAME TO notification;
ALTER TABLE notification_delivery_new RENAME TO notification_delivery;

CREATE INDEX notification_stream ON notification (member_id, id);
CREATE UNIQUE INDEX notification_member_dedup ON notification (member_id, dedup_key);
CREATE INDEX notification_unread ON notification (member_id) WHERE read_at IS NULL;
CREATE INDEX notification_created ON notification (created_at);

CREATE INDEX notification_delivery_due ON notification_delivery (state, next_attempt_at);
CREATE INDEX notification_delivery_channel ON notification_delivery (channel_id);

-- Members' email opt-ins take every member kind: `account_activate`, and
-- `request_revoked`, which 20261113100100 added to the notifications but
-- not here (opting in to it failed the CHECK). No table references this
-- one, so it is simply copied.
CREATE TABLE member_notification_pref_new (
    member_id INTEGER NOT NULL REFERENCES member (id) ON DELETE CASCADE,
    kind      TEXT NOT NULL CHECK (kind IN ('request_approved', 'request_rejected',
                  'request_revoked', 'request_partially_available', 'request_available',
                  'request_failed', 'follow_requested', 'account_activate')),
    email     INTEGER NOT NULL CHECK (email IN (0, 1)),
    PRIMARY KEY (member_id, kind)
) STRICT;

INSERT INTO member_notification_pref_new (member_id, kind, email)
    SELECT member_id, kind, email FROM member_notification_pref;

DROP TABLE member_notification_pref;
ALTER TABLE member_notification_pref_new RENAME TO member_notification_pref;
