-- Music inventory (spec 0006 slice 5, Q7 B, ADR 0013): albums of Navidrome
-- and of the music libraries of Jellyfin/Emby, by release-group MBID
-- (natural key `album:mbrg:<uuid>`), with their track count.
--
-- `inventory.kind` widens to `album`. The kind list is a CHECK, which
-- SQLite cannot alter, so the table is rebuilt: same columns, same rows,
-- same indexes, plus
-- * `tracks`: present tracks of an album; NULL for movies and series and
--   while gone. Not inside the `seasons` JSON: releases before this one
--   read `seasons` as season → episodes and would fail every inventory
--   read of an instance with albums. Album rows keep `seasons = '{}'`.
--
-- Rollback (ADR 0008): nothing references `inventory` (no foreign key
-- reaches into it), its rows are a cache that `inventory.sync` rebuilds,
-- and an older binary names its columns in every read and write, so it
-- neither sees `tracks` nor trips over it. It reads album rows like any
-- other (kind is text there) and, as it does not list music libraries,
-- marks them gone after two complete runs; the next `inventory.sync` of
-- this release lists them again (gone → present). An older binary
-- inserts no album rows, and reports a Navidrome instance in its
-- `inventory.sync` as failed (adapter unknown) without touching its rows.

CREATE TABLE inventory_new (
    service_instance_id INTEGER NOT NULL REFERENCES service_instance (id) ON DELETE CASCADE,
    natural_key         TEXT NOT NULL,
    kind                TEXT NOT NULL CHECK (kind IN ('movie', 'series', 'album')),
    tmdb_id             INTEGER,
    tvdb_id             INTEGER,
    imdb_id             TEXT,
    remote_id           TEXT NOT NULL,
    server_id           TEXT,
    title               TEXT NOT NULL,
    year                INTEGER,
    seasons             TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(seasons)),
    present_since       TEXT,
    last_seen           TEXT NOT NULL,
    missing_runs        INTEGER NOT NULL DEFAULT 0 CHECK (missing_runs >= 0),
    gone_since          TEXT,
    library_ids         TEXT
        CHECK (library_ids IS NULL OR (json_valid(library_ids) AND json_type(library_ids) = 'array')),
    tracks              INTEGER CHECK (tracks IS NULL OR (tracks >= 0 AND kind = 'album')),
    PRIMARY KEY (service_instance_id, natural_key),
    CHECK ((present_since IS NULL) <> (gone_since IS NULL)),
    -- Albums are keyed by their release group only.
    CHECK ((kind = 'album') = (natural_key GLOB 'album:mbrg:*'))
) STRICT;

INSERT INTO inventory_new (service_instance_id, natural_key, kind, tmdb_id, tvdb_id, imdb_id,
        remote_id, server_id, title, year, seasons, present_since, last_seen, missing_runs,
        gone_since, library_ids)
    SELECT service_instance_id, natural_key, kind, tmdb_id, tvdb_id, imdb_id, remote_id,
        server_id, title, year, seasons, present_since, last_seen, missing_runs, gone_since,
        library_ids
    FROM inventory;

DROP TABLE inventory;
ALTER TABLE inventory_new RENAME TO inventory;

CREATE INDEX inventory_tmdb_id ON inventory (tmdb_id) WHERE tmdb_id IS NOT NULL;
CREATE INDEX inventory_tvdb_id ON inventory (tvdb_id) WHERE tvdb_id IS NOT NULL;
CREATE INDEX inventory_imdb_id ON inventory (imdb_id) WHERE imdb_id IS NOT NULL;
-- Albums are found by their key across instances.
CREATE INDEX inventory_album_key ON inventory (natural_key) WHERE kind = 'album';
