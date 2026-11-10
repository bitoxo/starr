-- Lidarr as the music source (spec 0006 slice 3, Q6 A, ADR 0013): the
-- metadata profile a new artist gets, picked per category from the
-- instance's own profiles. Expand only (ADR 0008); every column is
-- nullable and an older binary ignores it.
--
-- * `source_lists.metadata_profiles`: the profiles as last read by
--   `sources.lists` (JSON, NULL for sources without them and for lists an
--   older binary stored).
-- * `service_role.metadata_profile_id`: the category's choice. Not inside
--   `settings`: releases before this one read that JSON with
--   `deny_unknown_fields` and would fail the whole category list.
-- * `acquisition.metadata_profile_id`: what an acquisition was approved
--   with, like its quality profile and root folder (D9).

ALTER TABLE source_lists ADD COLUMN metadata_profiles TEXT
    CHECK (metadata_profiles IS NULL OR json_valid(metadata_profiles));

ALTER TABLE service_role ADD COLUMN metadata_profile_id INTEGER
    CHECK (metadata_profile_id IS NULL OR kind = 'source');

ALTER TABLE acquisition ADD COLUMN metadata_profile_id INTEGER;
