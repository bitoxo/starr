-- Catalog model (domain model §5, spec 0003 slice 2). Shared: no member
-- columns; requests (slice 4) point here.
--
-- catalog_item: a movie, series or season, with Starr's own id and the
-- natural keys other systems use (TMDB, TVDB, IMDb; lesson 7). TMDB movie
-- and TV ids are separate number spaces, so keys are unique per kind.
-- Seasons hang off their series and are keyed by number. Metadata is a
-- cache: JSON with its provenance (which catalog adapter and instance) and
-- an expiry of at most six months after fetching (TMDB terms).
--
-- external_ref: item × service instance → the id the service uses, with the
-- last time it was confirmed. A cache, never the identity: the natural key
-- is the fallback when a remote id is stale or missing.

CREATE TABLE catalog_item (
    id                  INTEGER PRIMARY KEY,
    kind                TEXT NOT NULL CHECK (kind IN ('movie', 'series', 'season')),
    parent_id           INTEGER REFERENCES catalog_item (id) ON DELETE CASCADE,
    season_number       INTEGER CHECK (season_number IS NULL OR season_number >= 0),
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
    -- Seasons, and only seasons, have a parent and a number.
    CHECK ((kind = 'season') = (parent_id IS NOT NULL AND season_number IS NOT NULL)),
    CHECK (kind = 'season' OR parent_id IS NULL AND season_number IS NULL),
    -- Metadata comes with provenance and an expiry of at most 183 days.
    CHECK (metadata IS NULL OR (metadata_source IS NOT NULL
        AND metadata_fetched_at IS NOT NULL AND metadata_expires_at IS NOT NULL
        AND julianday(metadata_expires_at) - julianday(metadata_fetched_at) BETWEEN 0 AND 183))
) STRICT;

CREATE UNIQUE INDEX catalog_item_tmdb ON catalog_item (kind, tmdb_id)
    WHERE tmdb_id IS NOT NULL AND kind <> 'season';
CREATE UNIQUE INDEX catalog_item_tvdb ON catalog_item (kind, tvdb_id)
    WHERE tvdb_id IS NOT NULL AND kind <> 'season';
CREATE UNIQUE INDEX catalog_item_imdb ON catalog_item (kind, imdb_id)
    WHERE imdb_id IS NOT NULL AND kind <> 'season';
CREATE UNIQUE INDEX catalog_item_season ON catalog_item (parent_id, season_number)
    WHERE kind = 'season';
CREATE INDEX catalog_item_metadata_instance ON catalog_item (metadata_instance_id);
CREATE INDEX catalog_item_metadata_expires_at ON catalog_item (metadata_expires_at);

CREATE TABLE external_ref (
    catalog_item_id     INTEGER NOT NULL REFERENCES catalog_item (id) ON DELETE CASCADE,
    service_instance_id INTEGER NOT NULL REFERENCES service_instance (id) ON DELETE CASCADE,
    remote_id           TEXT NOT NULL CHECK (remote_id <> ''),
    confirmed_at        TEXT NOT NULL,
    PRIMARY KEY (catalog_item_id, service_instance_id)
) STRICT;

CREATE INDEX external_ref_remote ON external_ref (service_instance_id, remote_id);
