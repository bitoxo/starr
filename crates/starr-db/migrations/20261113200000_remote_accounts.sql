-- Foreign account inventory (spec 0005 D7, slice 1): every account a
-- consume service (Jellyfin, Emby) lists, per instance. Shared and admin
-- only; the basis for adoption suggestions. Written by `accounts.scan`,
-- which only reads the services.
--
-- Rows are never deleted by one observation: an account missing from
-- `missing_runs` complete listings in a row (2) gets `gone_since`; gone rows
-- are purged 30 days later. Incomplete listings never count towards
-- absence. `ignored_at`/`ignored_by` is a Starr-side flag set by an admin
-- (the admin's username as text, survives the admin's removal); it writes
-- nothing to the service. Rows go with their instance.
CREATE TABLE remote_account (
    service_instance_id INTEGER NOT NULL REFERENCES service_instance (id) ON DELETE CASCADE,
    -- The service's account ID; Starr's ownership key (D4).
    remote_id           TEXT NOT NULL,
    username            TEXT NOT NULL,
    enabled             INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    -- Administrator in the service: never touched by Starr (D2).
    privileged          INTEGER NOT NULL CHECK (privileged IN (0, 1)),
    -- Locked by the service after failed logins (D10).
    locked_by_service   INTEGER NOT NULL DEFAULT 0 CHECK (locked_by_service IN (0, 1)),
    -- NULL: every library; otherwise a JSON array of the service's library
    -- grant IDs.
    libraries           TEXT CHECK (libraries IS NULL OR json_valid(libraries)),
    first_seen          TEXT NOT NULL,
    last_seen           TEXT NOT NULL,
    missing_runs        INTEGER NOT NULL DEFAULT 0 CHECK (missing_runs >= 0),
    gone_since          TEXT,
    ignored_at          TEXT,
    ignored_by          TEXT CHECK (ignored_by IS NULL OR ignored_at IS NOT NULL),
    PRIMARY KEY (service_instance_id, remote_id)
) STRICT;

-- The last account scan per instance, for the admin page: when, whether the
-- listing was complete, and the error code if it failed (NULL on success).
ALTER TABLE service_instance ADD COLUMN accounts_scanned_at TEXT;
ALTER TABLE service_instance ADD COLUMN accounts_scan_complete INTEGER
    CHECK (accounts_scan_complete IS NULL OR accounts_scan_complete IN (0, 1));
ALTER TABLE service_instance ADD COLUMN accounts_scan_error TEXT;
