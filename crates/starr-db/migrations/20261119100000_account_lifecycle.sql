-- Account lifecycle, drift and hardening (spec 0005 slice 6). Expand only
-- (ADR 0008): nullable columns and one new table; an older binary ignores
-- all of them.
--
-- `service_account.password_attempt_generation`: the password generation
-- the worker is about to write into the account, stored before the write
-- and cleared with its result. A job retried after a crash between the
-- write and recording it finds the marker and does not write the same
-- password again (which would sign the member's devices out twice); the
-- member enters the password once more instead.
ALTER TABLE service_account ADD COLUMN password_attempt_generation INTEGER
    CHECK (password_attempt_generation IS NULL OR password_attempt_generation >= 0);

-- What Starr last left the account as (D13 drift baseline): enabled and
-- the library grant (NULL with `expected_enabled` set = every library;
-- otherwise a JSON array of grant IDs), from the read-back of Starr's last
-- write or a complete live run that found the account in sync. NULL until
-- then. A later difference was made in the service and is shown as a
-- deviation when Starr undoes it.
ALTER TABLE service_account ADD COLUMN expected_enabled INTEGER
    CHECK (expected_enabled IS NULL OR expected_enabled IN (0, 1));
ALTER TABLE service_account ADD COLUMN expected_libraries TEXT
    CHECK (expected_libraries IS NULL OR json_valid(expected_libraries));

-- Destructive account writes executed per instance (disable, libraries
-- taken away, delete): the destructive cap (D15) counts them over a
-- rolling hour, so a series of small runs cannot pass the cap one by one.
-- Rows older than a day are pruned; they go with their instance.
CREATE TABLE account_destructive_write (
    id                  INTEGER PRIMARY KEY,
    service_instance_id INTEGER NOT NULL REFERENCES service_instance (id) ON DELETE CASCADE,
    at                  TEXT NOT NULL
) STRICT;

CREATE INDEX account_destructive_write_at
    ON account_destructive_write (service_instance_id, at);
