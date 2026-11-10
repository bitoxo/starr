-- Consume services, read side (spec 0003 slice 6).
--
-- `public_url`: the address members reach a service's web client at, for
-- "open in" links, when it differs from the address Starr uses (e.g. Starr
-- talks to http://jellyfin:8096, members use https://watch.example.com).
-- NULL means links use `base_url`.
ALTER TABLE service_instance ADD COLUMN public_url TEXT;

-- Inventory (domain model §5, lesson 8): what a consume service has, by
-- natural key (`movie:tmdb:603`, `series:tvdb:81189`, …) per instance.
-- Rows are never deleted by observation: an item that disappeared keeps its
-- row with `gone_since`, and only after `missing_runs` complete listings
-- and a minimum time since `last_seen`. Rows go with their instance.
-- The link to `catalog_item` (spec 0003 slice 2) joins by the provider ID
-- columns.
CREATE TABLE inventory (
    service_instance_id INTEGER NOT NULL REFERENCES service_instance (id) ON DELETE CASCADE,
    natural_key         TEXT NOT NULL,
    kind                TEXT NOT NULL CHECK (kind IN ('movie', 'series')),
    tmdb_id             INTEGER,
    tvdb_id             INTEGER,
    imdb_id             TEXT,
    -- The service's item ID and server ID, for links (a cache).
    remote_id           TEXT NOT NULL,
    server_id           TEXT,
    title               TEXT NOT NULL,
    year                INTEGER,
    -- Series: JSON object season number -> present episodes, e.g. {"1": 10}.
    seasons             TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(seasons)),
    -- NULL while gone.
    present_since       TEXT,
    last_seen           TEXT NOT NULL,
    -- Complete listings in a row that did not contain the item.
    missing_runs        INTEGER NOT NULL DEFAULT 0 CHECK (missing_runs >= 0),
    -- NULL while present.
    gone_since          TEXT,
    PRIMARY KEY (service_instance_id, natural_key),
    CHECK ((present_since IS NULL) <> (gone_since IS NULL))
) STRICT;

CREATE INDEX inventory_tmdb_id ON inventory (tmdb_id) WHERE tmdb_id IS NOT NULL;
CREATE INDEX inventory_tvdb_id ON inventory (tvdb_id) WHERE tvdb_id IS NOT NULL;
CREATE INDEX inventory_imdb_id ON inventory (imdb_id) WHERE imdb_id IS NOT NULL;
