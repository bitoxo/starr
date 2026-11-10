-- Source settings kept per category and instance (spec 0008 ADM-02).
--
-- Removing or switching a category's source deletes its service_role row
-- and with it the quality profile, root folder, minimum availability and
-- metadata profile picked for it. They are kept here instead, so that
-- setting the same instance as the category's source again restores them
-- (re-adding is no longer a fresh setup). `settings` keeps the JSON shape
-- of service_role.settings, the metadata profile its own column (ADR 0013
-- amendment 2). Removing the instance forgets them.
--
-- Expand only (ADR 0008): a new table an older binary ignores.

CREATE TABLE kept_source_settings (
    category            TEXT NOT NULL REFERENCES category (name),
    service_instance_id INTEGER NOT NULL REFERENCES service_instance (id) ON DELETE CASCADE,
    settings            TEXT CHECK (settings IS NULL OR json_valid(settings)),
    metadata_profile_id INTEGER,
    PRIMARY KEY (category, service_instance_id)
) STRICT;

CREATE INDEX kept_source_settings_instance ON kept_source_settings (service_instance_id);
