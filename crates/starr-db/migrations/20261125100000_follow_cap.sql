-- Performer and studio follows with a weekly cap (spec 0007 slice 5, Q4).
--
-- request.follow_id: the follow that created a request (follows.check).
-- request.follow only says "asked with follow, or created by one" and the
-- request events name no follow, so the cap ("automatic requests per
-- follow per week") counts by this column. Requests made by hand, and all
-- requests from before this release, have none and never count.
--
-- follow.capped_scenes: how many new scenes the follow's last check left
-- beyond the cap (the hint "N more new scenes this week"); NULL when none.
--
-- service_role.follow_cap_per_week: the weekly cap of the private
-- category's catalog role (default 10 when NULL, 1 to 100). Its own column
-- and not the role's settings JSON (ADR 0013 amendment 2).
--
-- follow_capped: the member notification once per follow and week. The
-- notification tables are rebuilt as in 20261119100100 (CHECK constraints
-- cannot be altered), the member email preferences as in 20261118100100.
--
-- Expand only (ADR 0008): nullable columns an older binary ignores; it
-- reads follow_capped as an unknown kind and skips it.

ALTER TABLE request ADD COLUMN follow_id INTEGER REFERENCES follow (id) ON DELETE SET NULL;
CREATE INDEX request_follow ON request (follow_id, created_at) WHERE follow_id IS NOT NULL;

ALTER TABLE follow ADD COLUMN capped_scenes INTEGER
    CHECK (capped_scenes IS NULL OR capped_scenes > 0);

ALTER TABLE service_role ADD COLUMN follow_cap_per_week INTEGER
    CHECK (follow_cap_per_week IS NULL OR follow_cap_per_week BETWEEN 1 AND 100);

CREATE TABLE notification_new (
    id         INTEGER PRIMARY KEY,
    member_id  INTEGER REFERENCES member (id) ON DELETE CASCADE,
    kind       TEXT NOT NULL CHECK (kind IN ('request_approved', 'request_rejected',
                   'request_revoked', 'request_partially_available', 'request_available',
                   'request_failed', 'follow_requested', 'follow_capped', 'account_activate',
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

CREATE TABLE member_notification_pref_new (
    member_id INTEGER NOT NULL REFERENCES member (id) ON DELETE CASCADE,
    kind      TEXT NOT NULL CHECK (kind IN ('request_approved', 'request_rejected',
                  'request_revoked', 'request_partially_available', 'request_available',
                  'request_failed', 'follow_requested', 'follow_capped', 'account_activate')),
    email     INTEGER NOT NULL CHECK (email IN (0, 1)),
    PRIMARY KEY (member_id, kind)
) STRICT;

INSERT INTO member_notification_pref_new (member_id, kind, email)
    SELECT member_id, kind, email FROM member_notification_pref;

DROP TABLE member_notification_pref;
ALTER TABLE member_notification_pref_new RENAME TO member_notification_pref;
