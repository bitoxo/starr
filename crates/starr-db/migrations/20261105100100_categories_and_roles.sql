-- Categories and service roles (domain model §2–3, spec 0003 slice 2).
--
-- Categories are fixed in code; this table only holds whether the admin
-- switched one on. Service roles say what an instance does for a category:
-- `catalog` (discover, search, metadata), `source` (requests go here),
-- `consume` (members watch here). Rules kept by the database:
--   * at most one source per category (unique index below; slice 4 adds
--     inactive sources that finish open requests),
--   * at most one default catalog per category (unique index); the
--     repository keeps "exactly one while any catalog exists",
--   * `is_default` only on catalogs.
-- The capability rule (catalog needs `metadata`, source `requests`, consume
-- `accounts`) depends on adapter descriptors in code and is checked by the
-- repository when a role is added.
--
-- `settings` is JSON per kind: catalog → the relevance (NULL until the admin
-- saved one; Discover needs it, D3); source → quality profile, root folder,
-- minimum availability (D9); consume → NULL for now.

CREATE TABLE category (
    name       TEXT PRIMARY KEY CHECK (name IN ('movies', 'series', 'music', 'adult')),
    enabled    INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0, 1)),
    updated_at TEXT NOT NULL
) STRICT;

INSERT INTO category (name, enabled, updated_at) VALUES
    ('movies', 0, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    ('series', 0, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    ('music', 0, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    ('adult', 0, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));

CREATE TABLE service_role (
    id                  INTEGER PRIMARY KEY,
    category            TEXT NOT NULL REFERENCES category (name),
    kind                TEXT NOT NULL CHECK (kind IN ('catalog', 'source', 'consume')),
    service_instance_id INTEGER NOT NULL REFERENCES service_instance (id) ON DELETE CASCADE,
    is_default          INTEGER NOT NULL DEFAULT 0 CHECK (is_default IN (0, 1)),
    settings            TEXT CHECK (settings IS NULL OR json_valid(settings)),
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    UNIQUE (category, kind, service_instance_id),
    CHECK (is_default = 0 OR kind = 'catalog')
) STRICT;

CREATE UNIQUE INDEX service_role_one_source ON service_role (category) WHERE kind = 'source';
CREATE UNIQUE INDEX service_role_one_default_catalog
    ON service_role (category) WHERE kind = 'catalog' AND is_default = 1;
CREATE INDEX service_role_service_instance ON service_role (service_instance_id);

-- Quality profiles and root folders of a source instance as last read by the
-- `sources.lists` job (lesson 32: read, not remembered; the admin picks from
-- these and the API validates against them). `error` holds the taxonomy code
-- of a failed read; the lists of the last successful read stay.
CREATE TABLE source_lists (
    service_instance_id INTEGER PRIMARY KEY REFERENCES service_instance (id) ON DELETE CASCADE,
    quality_profiles    TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(quality_profiles)),
    root_folders        TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(root_folders)),
    fetched_at          TEXT,
    error               TEXT,
    attempted_at        TEXT NOT NULL
) STRICT;
