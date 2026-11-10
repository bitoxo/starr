-- Removal of scenes (spec 0007 slice 4, Q5 A): adult availability comes
-- from Whisparr's files only, so "removed" (spec 0004 R4) has only
-- Whisparr's evidence. acquisitions.sync looks at an available scene again
-- every six hours; acquisition.file_missing_since is when it first found
-- no file there. Found without a file again on a later run, the scene is
-- removed and can be requested again; a file found clears it.
--
-- Expand only (ADR 0008): a nullable column an older binary ignores.

ALTER TABLE acquisition ADD COLUMN file_missing_since TEXT;
