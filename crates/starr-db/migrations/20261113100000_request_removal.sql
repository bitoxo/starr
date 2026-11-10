-- Removing requests (spec 0004).
--
-- request_item.decision gains `revoked` (an admin revoked it, R3) and
-- `removed` (the unit left the library, R4); `cancelled` now also covers a
-- member cancelling after approval (R2). Ended items keep the link to the
-- acquisition they had (the journey shows how far they got), so the link
-- CHECK allows every decision that was approved once. `ended_at` is when
-- an item was withdrawn, cancelled, revoked or removed; `decided_at` keeps
-- the approval time.
--
-- request gains the states `revoked` and `removed`, the admin's
-- `revoke_reason` (shown to the member like a reject reason) and
-- `hidden_at`: the member hid the finished request from their own list
-- (admins and the audit log still see it; tombstone, no hard delete).
--
-- acquisition gains the state `cancelled` (nobody wants it any more;
-- nothing is sent for it), what Starr changed in the source when it
-- submitted it (`starr_added`: Starr added the title; `starr_monitored`:
-- Starr added it or switched the unit's monitoring on), `stop_requested_at`
-- (a stop waits for `requests.submit`: unmonitor exactly what Starr turned
-- on) and `stopped_at` (when that stop went out). Existing acquisitions
-- get 0 for both flags: unknown counts as "not changed by Starr", so Starr
-- never unmonitors a title it cannot prove it monitored itself (D14).
--
-- The state and decision lists are CHECK constraints, which SQLite cannot
-- alter: acquisition, request, request_item and request_event are rebuilt.
-- Foreign keys stay on inside the migration's transaction and dropping a
-- table deletes its rows first (cascading into request_item and
-- request_event), so every table is copied before anything is dropped: the
-- new tables reference each other, the old ones are dropped child first,
-- and the renames carry the references over to the final names.

CREATE TABLE acquisition_new (
    id                   INTEGER PRIMARY KEY,
    catalog_item_id      INTEGER NOT NULL REFERENCES catalog_item (id),
    service_instance_id  INTEGER REFERENCES service_instance (id) ON DELETE SET NULL,
    category             TEXT NOT NULL CHECK (category IN ('movies', 'series', 'music', 'adult')),
    state                TEXT NOT NULL CHECK (state IN ('pending', 'submitted', 'downloading',
                             'partially_available', 'available', 'failed', 'cancelled')),
    quality_profile_id   INTEGER NOT NULL,
    root_folder_path     TEXT NOT NULL CHECK (root_folder_path <> ''),
    minimum_availability TEXT CHECK (minimum_availability IS NULL
                             OR minimum_availability IN ('announced', 'inCinemas', 'released')),
    progress             REAL CHECK (progress IS NULL OR progress BETWEEN 0 AND 1),
    source_message       TEXT,
    error_code           TEXT,
    last_error           TEXT,
    remote_id            TEXT,
    submit_attempts      INTEGER NOT NULL DEFAULT 0 CHECK (submit_attempts >= 0),
    next_attempt_at      TEXT,
    submitted_at         TEXT,
    available_at         TEXT,
    synced_at            TEXT,
    created_at           TEXT NOT NULL,
    updated_at           TEXT NOT NULL,
    download_stage       TEXT
        CHECK (download_stage IS NULL OR download_stage IN ('importing', 'arriving')),
    download_stage_since TEXT,
    starr_added          INTEGER NOT NULL DEFAULT 0 CHECK (starr_added IN (0, 1)),
    starr_monitored      INTEGER NOT NULL DEFAULT 0 CHECK (starr_monitored IN (0, 1)),
    stop_requested_at    TEXT,
    stopped_at           TEXT,
    UNIQUE (catalog_item_id, service_instance_id),
    CHECK (state <> 'failed' OR last_error IS NOT NULL),
    -- Starr can only have added what it also monitors.
    CHECK (starr_added = 0 OR starr_monitored = 1)
) STRICT;

INSERT INTO acquisition_new (id, catalog_item_id, service_instance_id, category, state,
        quality_profile_id, root_folder_path, minimum_availability, progress, source_message,
        error_code, last_error, remote_id, submit_attempts, next_attempt_at, submitted_at,
        available_at, synced_at, created_at, updated_at, download_stage, download_stage_since)
    SELECT id, catalog_item_id, service_instance_id, category, state, quality_profile_id,
        root_folder_path, minimum_availability, progress, source_message, error_code,
        last_error, remote_id, submit_attempts, next_attempt_at, submitted_at, available_at,
        synced_at, created_at, updated_at, download_stage, download_stage_since
    FROM acquisition;

CREATE TABLE request_new (
    id              INTEGER PRIMARY KEY,
    member_id       INTEGER NOT NULL REFERENCES member (id) ON DELETE CASCADE,
    catalog_item_id INTEGER NOT NULL REFERENCES catalog_item (id),
    category        TEXT NOT NULL CHECK (category IN ('movies', 'series', 'music', 'adult')),
    state           TEXT NOT NULL CHECK (state IN ('pending_approval', 'approved', 'submitted',
                        'in_progress', 'partially_available', 'available', 'rejected',
                        'cancelled', 'failed', 'revoked', 'removed')),
    reject_reason   TEXT CHECK (reject_reason IS NULL OR length(reject_reason) BETWEEN 1 AND 500),
    follow          INTEGER NOT NULL DEFAULT 0 CHECK (follow IN (0, 1)),
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    revoke_reason   TEXT CHECK (revoke_reason IS NULL OR length(revoke_reason) BETWEEN 1 AND 500),
    hidden_at       TEXT,
    UNIQUE (id, member_id)
) STRICT;

INSERT INTO request_new (id, member_id, catalog_item_id, category, state, reject_reason,
        follow, created_at, updated_at)
    SELECT id, member_id, catalog_item_id, category, state, reject_reason, follow, created_at,
        updated_at
    FROM request;

CREATE TABLE request_item_new (
    id              INTEGER PRIMARY KEY,
    request_id      INTEGER NOT NULL,
    member_id       INTEGER NOT NULL REFERENCES member (id) ON DELETE CASCADE,
    catalog_item_id INTEGER NOT NULL REFERENCES catalog_item (id),
    season_number   INTEGER CHECK (season_number IS NULL OR season_number >= 0),
    decision        TEXT NOT NULL CHECK (decision IN ('pending_approval', 'approved',
                        'rejected', 'cancelled', 'revoked', 'removed')),
    acquisition_id  INTEGER REFERENCES acquisition_new (id),
    decided_at      TEXT,
    ended_at        TEXT,
    FOREIGN KEY (request_id, member_id) REFERENCES request_new (id, member_id) ON DELETE CASCADE,
    -- Linked once approved; ended items keep the link.
    CHECK (acquisition_id IS NULL
           OR decision IN ('approved', 'cancelled', 'revoked', 'removed')),
    -- `removed` only ever follows an approval.
    CHECK (decision <> 'removed' OR acquisition_id IS NOT NULL)
) STRICT;

INSERT INTO request_item_new (id, request_id, member_id, catalog_item_id, season_number,
        decision, acquisition_id, decided_at)
    SELECT id, request_id, member_id, catalog_item_id, season_number, decision, acquisition_id,
        decided_at
    FROM request_item;

CREATE TABLE request_event_new (
    id         INTEGER PRIMARY KEY,
    request_id INTEGER NOT NULL REFERENCES request_new (id) ON DELETE CASCADE,
    at         TEXT NOT NULL,
    from_state TEXT,
    to_state   TEXT NOT NULL,
    trigger    TEXT NOT NULL CHECK (trigger IN ('member', 'admin', 'system')),
    actor      TEXT,
    detail     TEXT
) STRICT;

INSERT INTO request_event_new (id, request_id, at, from_state, to_state, trigger, actor, detail)
    SELECT id, request_id, at, from_state, to_state, trigger, actor, detail FROM request_event;

DROP TABLE request_event;
DROP TABLE request_item;
DROP TABLE request;
DROP TABLE acquisition;

ALTER TABLE acquisition_new RENAME TO acquisition;
ALTER TABLE request_new RENAME TO request;
ALTER TABLE request_item_new RENAME TO request_item;
ALTER TABLE request_event_new RENAME TO request_event;

CREATE INDEX acquisition_service_instance ON acquisition (service_instance_id);
CREATE INDEX acquisition_state ON acquisition (state, next_attempt_at);
CREATE INDEX acquisition_stop ON acquisition (stop_requested_at)
    WHERE stop_requested_at IS NOT NULL;

CREATE INDEX request_member ON request (member_id, created_at);
CREATE INDEX request_state ON request (state, created_at);
CREATE INDEX request_catalog_item ON request (catalog_item_id);
CREATE INDEX request_category ON request (category);

CREATE UNIQUE INDEX request_item_one_open ON request_item (member_id, catalog_item_id)
    WHERE decision IN ('pending_approval', 'approved');
CREATE INDEX request_item_request ON request_item (request_id);
CREATE INDEX request_item_catalog_item ON request_item (catalog_item_id, decision);
CREATE INDEX request_item_acquisition ON request_item (acquisition_id);

CREATE INDEX request_event_request ON request_event (request_id, id);
