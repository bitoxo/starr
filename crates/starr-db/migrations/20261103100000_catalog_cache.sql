-- Catalog response cache (spec 0003 slice 1): bodies of metadata service
-- responses (TMDB), keyed by instance and request (path plus query, never
-- the credential). Kept for the response's Cache-Control lifetime, at most
-- six months (TMDB terms); housekeeping deletes expired rows and caps the
-- table. Rows go with their service instance.

CREATE TABLE catalog_cache (
    service_instance_id INTEGER NOT NULL REFERENCES service_instance (id) ON DELETE CASCADE,
    request             TEXT NOT NULL,
    body                BLOB NOT NULL,
    fetched_at          TEXT NOT NULL,
    expires_at          TEXT NOT NULL,
    PRIMARY KEY (service_instance_id, request)
) STRICT;

CREATE INDEX catalog_cache_expires_at ON catalog_cache (expires_at);
