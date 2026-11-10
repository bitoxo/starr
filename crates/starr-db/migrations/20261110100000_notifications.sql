-- Notifications (spec 0003 O1, O2, D16; domain model §7).
--
-- notification: one item in a stream. `member_id` names the member's
-- personal stream; NULL is the admin stream (shared by all admins: one
-- read mark for everyone). Content is structured (`kind` + JSON payload)
-- and rendered only where it is shown or sent (lesson 30). `dedup_key` is
-- unique per stream, so a retried transaction or a state that flaps back and forth
-- never notifies twice for the same thing (one notification per request
-- transition kind). Unique per stream: an index with the owner for member
-- streams (lesson 4); the admin stream (NULL owner) is checked on insert
-- inside the write transaction.
--
-- notification_channel: channels an admin configured. One email (SMTP)
-- channel serves member emails (and the admin stream if it names an admin
-- address); webhook and Discord channels serve the admin stream. Secrets
-- (SMTP password, webhook URL: Discord's carries its token) are sealed to
-- the worker's secret.key like service credentials (ADR 0003 amendment 1):
-- the web process stores them but never opens them. `settings` holds the
-- non-secret part as JSON; `admin_events` the admin kinds sent there.
-- `last_*` is the outcome of the latest delivery or test (live state).
--
-- member_notification_pref: per member and kind, whether it also goes by
-- email. In-app is always on; no row means email off (opt-in).
--
-- notification_delivery: one notification on one channel, with its own
-- retries (`notifications.deliver`, backoff per attempt).

CREATE TABLE notification (
    id         INTEGER PRIMARY KEY,
    member_id  INTEGER REFERENCES member (id) ON DELETE CASCADE,
    kind       TEXT NOT NULL CHECK (kind IN ('request_approved', 'request_rejected',
                   'request_partially_available', 'request_available', 'request_failed',
                   'follow_requested', 'approval_pending', 'acquisition_failed')),
    payload    TEXT NOT NULL CHECK (json_valid(payload)),
    dedup_key  TEXT NOT NULL,
    created_at TEXT NOT NULL,
    read_at    TEXT,
    -- Member kinds go to a member, admin kinds to the admin stream.
    CHECK ((member_id IS NULL) = (kind IN ('approval_pending', 'acquisition_failed')))
) STRICT;

CREATE INDEX notification_stream ON notification (member_id, id);
CREATE UNIQUE INDEX notification_member_dedup ON notification (member_id, dedup_key);
CREATE INDEX notification_unread ON notification (member_id) WHERE read_at IS NULL;
CREATE INDEX notification_created ON notification (created_at);

CREATE TABLE notification_channel (
    id                INTEGER PRIMARY KEY,
    kind              TEXT NOT NULL CHECK (kind IN ('email', 'webhook', 'discord')),
    name              TEXT NOT NULL UNIQUE COLLATE NOCASE CHECK (length(name) BETWEEN 1 AND 100),
    enabled           INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    settings          TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(settings)),
    secret_ciphertext BLOB,
    secret_recipient  TEXT,
    admin_events      TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(admin_events)),
    last_attempt_at   TEXT,
    last_ok           INTEGER CHECK (last_ok IS NULL OR last_ok IN (0, 1)),
    last_error_code   TEXT,
    last_error        TEXT,
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL,
    CHECK ((secret_ciphertext IS NULL) = (secret_recipient IS NULL)),
    -- Webhooks are nothing without their URL.
    CHECK (kind = 'email' OR secret_ciphertext IS NOT NULL)
) STRICT;

-- One SMTP server for all member emails.
CREATE UNIQUE INDEX notification_channel_one_email ON notification_channel (kind)
    WHERE kind = 'email';

CREATE TABLE member_notification_pref (
    member_id INTEGER NOT NULL REFERENCES member (id) ON DELETE CASCADE,
    kind      TEXT NOT NULL CHECK (kind IN ('request_approved', 'request_rejected',
                  'request_partially_available', 'request_available', 'request_failed',
                  'follow_requested')),
    email     INTEGER NOT NULL CHECK (email IN (0, 1)),
    PRIMARY KEY (member_id, kind)
) STRICT;

CREATE TABLE notification_delivery (
    id              INTEGER PRIMARY KEY,
    notification_id INTEGER NOT NULL REFERENCES notification (id) ON DELETE CASCADE,
    channel_id      INTEGER NOT NULL REFERENCES notification_channel (id) ON DELETE CASCADE,
    state           TEXT NOT NULL CHECK (state IN ('pending', 'delivered', 'failed')),
    attempts        INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    -- Pending: due from then on.
    next_attempt_at TEXT,
    error_code      TEXT,
    last_error      TEXT,
    delivered_at    TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    UNIQUE (notification_id, channel_id),
    CHECK (state <> 'failed' OR last_error IS NOT NULL)
) STRICT;

CREATE INDEX notification_delivery_due ON notification_delivery (state, next_attempt_at);
CREATE INDEX notification_delivery_channel ON notification_delivery (channel_id);
