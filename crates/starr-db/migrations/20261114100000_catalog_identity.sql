-- Catalog identity (ADR 0012).
--
-- catalog_item.kind widens to the kinds music and adult need: `movie`,
-- `series`, `season`, `artist`, `album`, `scene`, `performer`, `studio`
-- (adult full-length movies reuse `movie`). Generic parent rule: `season`
-- and `album` hang off a parent, the others do not; only `season` has a
-- number.
--
-- catalog_key: the natural keys of an item, one per scheme (`tmdb_movie`,
-- `tmdb_tv`, `tvdb` = TVDB series id, `imdb`, `mb_artist`,
-- `mb_release_group`, `stashdb_scene`, `stashdb_performer`,
-- `stashdb_studio`, …). A key names at most one item. The scheme list
-- lives in Rust, so a new scheme needs no rebuild; the CHECKs only keep
-- the value format of the known ones. Expand step of expand/contract (ADR
-- 0008): the key columns of catalog_item stay and are still written, but
-- read only from here; they are dropped one release later. The backfill
-- below copies them; where two items claimed one key (the old unique
-- indexes were per kind), the older item keeps it.
--
-- follow gains `known_through` (a date cutoff for artists, performers and
-- studios; phase 3) and `seasons_through` becomes nullable; one of them is
-- set. Phase 1 code keeps writing `seasons_through`.
--
-- The kind list is a CHECK, which SQLite cannot alter: catalog_item is
-- rebuilt, and with it every table that references it (external_ref,
-- acquisition, request, request_item, request_event via request, follow).
-- Foreign keys stay on inside the migration's transaction and dropping a
-- table deletes its rows first (cascading into its children), so every
-- table is copied before anything is dropped: the new tables reference
-- each other, the old ones are dropped child first, and the renames carry
-- the references over to the final names. Ids are kept, nothing else in
-- the schema points at these tables.

CREATE TABLE catalog_item_new (
    id                  INTEGER PRIMARY KEY,
    kind                TEXT NOT NULL CHECK (kind IN ('movie', 'series', 'season', 'artist',
                            'album', 'scene', 'performer', 'studio')),
    parent_id           INTEGER REFERENCES catalog_item_new (id) ON DELETE CASCADE,
    season_number       INTEGER CHECK (season_number IS NULL OR season_number >= 0),
    -- Natural key columns of phase 1: written until the contract release,
    -- read from catalog_key.
    tmdb_id             INTEGER CHECK (tmdb_id IS NULL OR tmdb_id > 0),
    tvdb_id             INTEGER CHECK (tvdb_id IS NULL OR tvdb_id > 0),
    imdb_id             TEXT CHECK (imdb_id IS NULL OR imdb_id GLOB 'tt[0-9]*'),
    title               TEXT NOT NULL,
    year                INTEGER,
    poster_path         TEXT,
    metadata            TEXT CHECK (metadata IS NULL OR json_valid(metadata)),
    -- Provenance: adapter type and instance the metadata came from.
    metadata_source     TEXT,
    metadata_instance_id INTEGER REFERENCES service_instance (id) ON DELETE SET NULL,
    metadata_fetched_at TEXT,
    metadata_expires_at TEXT,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    -- Seasons and albums, and only they, have a parent.
    CHECK ((kind IN ('season', 'album')) = (parent_id IS NOT NULL)),
    -- Only seasons have a number.
    CHECK ((kind = 'season') = (season_number IS NOT NULL)),
    -- Metadata comes with provenance and an expiry of at most 183 days.
    CHECK (metadata IS NULL OR (metadata_source IS NOT NULL
        AND metadata_fetched_at IS NOT NULL AND metadata_expires_at IS NOT NULL
        AND julianday(metadata_expires_at) - julianday(metadata_fetched_at) BETWEEN 0 AND 183))
) STRICT;

INSERT INTO catalog_item_new (id, kind, parent_id, season_number, tmdb_id, tvdb_id, imdb_id,
        title, year, poster_path, metadata, metadata_source, metadata_instance_id,
        metadata_fetched_at, metadata_expires_at, created_at, updated_at)
    SELECT id, kind, parent_id, season_number, tmdb_id, tvdb_id, imdb_id, title, year,
        poster_path, metadata, metadata_source, metadata_instance_id, metadata_fetched_at,
        metadata_expires_at, created_at, updated_at
    FROM catalog_item;

CREATE TABLE catalog_key (
    catalog_item_id INTEGER NOT NULL REFERENCES catalog_item_new (id) ON DELETE CASCADE,
    scheme          TEXT NOT NULL CHECK (scheme <> '' AND scheme NOT GLOB '*[^a-z0-9_]*'),
    value           TEXT NOT NULL CHECK (value <> ''),
    PRIMARY KEY (scheme, value),
    UNIQUE (catalog_item_id, scheme),
    -- Numeric ids without leading zeros.
    CHECK (scheme NOT IN ('tmdb_movie', 'tmdb_tv', 'tvdb', 'tpdb_movie')
           OR (value NOT GLOB '*[^0-9]*' AND value NOT GLOB '0*')),
    CHECK (scheme <> 'imdb' OR (value GLOB 'tt[0-9]*' AND substr(value, 3) NOT GLOB '*[^0-9]*')),
    -- Lower-case hyphenated UUIDs.
    CHECK (scheme NOT IN ('mb_artist', 'mb_release_group', 'stashdb_scene',
                          'stashdb_performer', 'stashdb_studio')
           OR (length(value) = 36 AND value GLOB
               '[0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]-[0-9a-f][0-9a-f][0-9a-f][0-9a-f]-[0-9a-f][0-9a-f][0-9a-f][0-9a-f]-[0-9a-f][0-9a-f][0-9a-f][0-9a-f]-[0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]'))
) STRICT;

-- Backfill, oldest item first: OR IGNORE keeps the first claim of a key.
INSERT OR IGNORE INTO catalog_key (catalog_item_id, scheme, value)
    SELECT id, iif(kind = 'movie', 'tmdb_movie', 'tmdb_tv'), CAST(tmdb_id AS TEXT)
    FROM catalog_item_new WHERE tmdb_id IS NOT NULL AND kind IN ('movie', 'series')
    ORDER BY id;
INSERT OR IGNORE INTO catalog_key (catalog_item_id, scheme, value)
    SELECT id, 'tvdb', CAST(tvdb_id AS TEXT)
    FROM catalog_item_new WHERE tvdb_id IS NOT NULL AND kind = 'series'
    ORDER BY id;
INSERT OR IGNORE INTO catalog_key (catalog_item_id, scheme, value)
    SELECT id, 'imdb', imdb_id
    FROM catalog_item_new WHERE imdb_id IS NOT NULL AND kind IN ('movie', 'series')
    ORDER BY id;

CREATE TABLE external_ref_new (
    catalog_item_id     INTEGER NOT NULL REFERENCES catalog_item_new (id) ON DELETE CASCADE,
    service_instance_id INTEGER NOT NULL REFERENCES service_instance (id) ON DELETE CASCADE,
    remote_id           TEXT NOT NULL CHECK (remote_id <> ''),
    confirmed_at        TEXT NOT NULL,
    PRIMARY KEY (catalog_item_id, service_instance_id)
) STRICT;

INSERT INTO external_ref_new (catalog_item_id, service_instance_id, remote_id, confirmed_at)
    SELECT catalog_item_id, service_instance_id, remote_id, confirmed_at FROM external_ref;

CREATE TABLE acquisition_new (
    id                   INTEGER PRIMARY KEY,
    catalog_item_id      INTEGER NOT NULL REFERENCES catalog_item_new (id),
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
        available_at, synced_at, created_at, updated_at, download_stage, download_stage_since,
        starr_added, starr_monitored, stop_requested_at, stopped_at)
    SELECT id, catalog_item_id, service_instance_id, category, state, quality_profile_id,
        root_folder_path, minimum_availability, progress, source_message, error_code,
        last_error, remote_id, submit_attempts, next_attempt_at, submitted_at, available_at,
        synced_at, created_at, updated_at, download_stage, download_stage_since, starr_added,
        starr_monitored, stop_requested_at, stopped_at
    FROM acquisition;

CREATE TABLE request_new (
    id              INTEGER PRIMARY KEY,
    member_id       INTEGER NOT NULL REFERENCES member (id) ON DELETE CASCADE,
    catalog_item_id INTEGER NOT NULL REFERENCES catalog_item_new (id),
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
        follow, created_at, updated_at, revoke_reason, hidden_at)
    SELECT id, member_id, catalog_item_id, category, state, reject_reason, follow, created_at,
        updated_at, revoke_reason, hidden_at
    FROM request;

CREATE TABLE request_item_new (
    id              INTEGER PRIMARY KEY,
    request_id      INTEGER NOT NULL,
    member_id       INTEGER NOT NULL REFERENCES member (id) ON DELETE CASCADE,
    catalog_item_id INTEGER NOT NULL REFERENCES catalog_item_new (id),
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
        decision, acquisition_id, decided_at, ended_at)
    SELECT id, request_id, member_id, catalog_item_id, season_number, decision, acquisition_id,
        decided_at, ended_at
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

CREATE TABLE follow_new (
    id              INTEGER PRIMARY KEY,
    member_id       INTEGER NOT NULL REFERENCES member (id) ON DELETE CASCADE,
    -- The series (later: artist, performer, studio).
    catalog_item_id INTEGER NOT NULL REFERENCES catalog_item_new (id),
    category        TEXT NOT NULL CHECK (category IN ('movies', 'series', 'music', 'adult')),
    -- Series: the highest season known when following started or moved up.
    seasons_through INTEGER CHECK (seasons_through IS NULL OR seasons_through >= 0),
    -- Artists, performers, studios: releases after this date are new.
    known_through   TEXT,
    created_at      TEXT NOT NULL,
    -- The last run of `follows.check` that read the catalog for it.
    checked_at      TEXT,
    ended_at        TEXT,
    CHECK (seasons_through IS NOT NULL OR known_through IS NOT NULL),
    CHECK (ended_at IS NULL OR ended_at >= created_at)
) STRICT;

INSERT INTO follow_new (id, member_id, catalog_item_id, category, seasons_through, created_at,
        checked_at, ended_at)
    SELECT id, member_id, catalog_item_id, category, seasons_through, created_at, checked_at,
        ended_at
    FROM follow;

DROP TABLE request_event;
DROP TABLE request_item;
DROP TABLE request;
DROP TABLE follow;
DROP TABLE acquisition;
DROP TABLE external_ref;
DROP TABLE catalog_item;

ALTER TABLE catalog_item_new RENAME TO catalog_item;
ALTER TABLE external_ref_new RENAME TO external_ref;
ALTER TABLE acquisition_new RENAME TO acquisition;
ALTER TABLE request_new RENAME TO request;
ALTER TABLE request_item_new RENAME TO request_item;
ALTER TABLE request_event_new RENAME TO request_event;
ALTER TABLE follow_new RENAME TO follow;

CREATE UNIQUE INDEX catalog_item_tmdb ON catalog_item (kind, tmdb_id)
    WHERE tmdb_id IS NOT NULL AND kind <> 'season';
CREATE UNIQUE INDEX catalog_item_tvdb ON catalog_item (kind, tvdb_id)
    WHERE tvdb_id IS NOT NULL AND kind <> 'season';
CREATE UNIQUE INDEX catalog_item_imdb ON catalog_item (kind, imdb_id)
    WHERE imdb_id IS NOT NULL AND kind <> 'season';
CREATE UNIQUE INDEX catalog_item_season ON catalog_item (parent_id, season_number)
    WHERE kind = 'season';
CREATE INDEX catalog_item_parent ON catalog_item (parent_id) WHERE parent_id IS NOT NULL;
CREATE INDEX catalog_item_metadata_instance ON catalog_item (metadata_instance_id);
CREATE INDEX catalog_item_metadata_expires_at ON catalog_item (metadata_expires_at);

CREATE INDEX external_ref_remote ON external_ref (service_instance_id, remote_id);

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

CREATE UNIQUE INDEX follow_one_active ON follow (member_id, catalog_item_id)
    WHERE ended_at IS NULL;
CREATE INDEX follow_member ON follow (member_id, created_at);
CREATE INDEX follow_catalog_item ON follow (catalog_item_id);
CREATE INDEX follow_active ON follow (id) WHERE ended_at IS NULL;
