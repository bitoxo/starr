-- Libraries per category (spec 0005 M4, slice 2).
--
-- `service_library`: the libraries a consume instance listed in its last
-- library listing (written by `accounts.scan`, which only reads the
-- service). The admin maps them to categories in the consume role's
-- `settings` (`{"libraries": [{"id": …, "grantId": …}]}`); the API
-- validates a mapping against these rows. `library_id` is the service's
-- library ID item queries and the inventory use (Jellyfin and Emby:
-- `ItemId`), `grant_id` the ID account grants name (Emby: the library
-- `Guid`; Jellyfin: the `ItemId` again). `content` is the library's
-- collection type as Starr reads it (`movies`, `series`, `music`, `books`,
-- `other`); no CHECK, so a newer release may add values (ADR 0012).
-- A complete listing removes rows it no longer lists; an incomplete one
-- only adds and updates. `ignored_at`/`ignored_by`: an admin decided the
-- library belongs to no category (the admin's username as text); it is
-- then no longer an "unmapped library" under "needs you". Starr-side
-- only. Rows go with their instance.
CREATE TABLE service_library (
    service_instance_id INTEGER NOT NULL REFERENCES service_instance (id) ON DELETE CASCADE,
    library_id          TEXT NOT NULL CHECK (library_id <> ''),
    grant_id            TEXT NOT NULL CHECK (grant_id <> ''),
    name                TEXT NOT NULL,
    content             TEXT NOT NULL,
    first_seen          TEXT NOT NULL,
    last_seen           TEXT NOT NULL,
    ignored_at          TEXT,
    ignored_by          TEXT CHECK (ignored_by IS NULL OR ignored_at IS NOT NULL),
    PRIMARY KEY (service_instance_id, library_id)
) STRICT;

-- The last library listing per instance: when, whether it was complete,
-- and the error code if it failed (NULL on success; the rows of the last
-- successful listing stay).
ALTER TABLE service_instance ADD COLUMN libraries_listed_at TEXT;
ALTER TABLE service_instance ADD COLUMN libraries_complete INTEGER
    CHECK (libraries_complete IS NULL OR libraries_complete IN (0, 1));
ALTER TABLE service_instance ADD COLUMN libraries_error TEXT;

-- Inventory by library (ADR 0012, known gap): the library IDs an item was
-- listed in (JSON array of `library_id`s). Availability for a category
-- counts only items in the libraries mapped to it on that instance, so an
-- adult film with a TMDB ID never counts as an available movie. NULL for
-- rows written before this release and for rows an older binary inserts
-- after a rollback (its upserts leave the column of existing rows as it
-- was) until the next `inventory.sync`; NULL rows count as they did
-- before, for consume roles without a mapping only. Saving a mapping
-- queues a sync.
ALTER TABLE inventory ADD COLUMN library_ids TEXT
    CHECK (library_ids IS NULL OR (json_valid(library_ids) AND json_type(library_ids) = 'array'));
