-- Admin notifications about managed accounts (spec 0005 slice 6, D2, D10,
-- D13): `account_privileged` (an account Starr manages gained
-- administrator rights in the service; Starr keeps its hands off) and
-- `account_locked` (the service locked it after failed logins; never
-- lifted by Starr). Both join the admin kinds; the payload names the
-- service (`title`) and the member (`member`).
--
-- Rebuilt the same way as 20261118100100 (CHECK constraints cannot be
-- altered): both tables copied, the old ones dropped child first, the
-- renames carry the reference over. An older binary reads the new kinds as
-- unknown and skips them.

CREATE TABLE notification_new (
    id         INTEGER PRIMARY KEY,
    member_id  INTEGER REFERENCES member (id) ON DELETE CASCADE,
    kind       TEXT NOT NULL CHECK (kind IN ('request_approved', 'request_rejected',
                   'request_revoked', 'request_partially_available', 'request_available',
                   'request_failed', 'follow_requested', 'account_activate',
                   'approval_pending', 'acquisition_failed', 'import_blocked',
                   'account_privileged', 'account_locked')),
    payload    TEXT NOT NULL CHECK (json_valid(payload)),
    dedup_key  TEXT NOT NULL,
    created_at TEXT NOT NULL,
    read_at    TEXT,
    -- Member kinds go to a member, admin kinds to the admin stream.
    CHECK ((member_id IS NULL)
           = (kind IN ('approval_pending', 'acquisition_failed', 'import_blocked',
                       'account_privileged', 'account_locked')))
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
