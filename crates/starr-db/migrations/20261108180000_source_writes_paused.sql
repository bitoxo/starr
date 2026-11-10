-- Paused writes per source instance (spec 0003, maintainer 2026-11-02).
--
-- While `writes_paused_at` is set, `requests.submit` sends nothing to the
-- instance (no add, no monitoring, no search): approvals still work and
-- their pending acquisitions are held until an admin resumes the writes.
-- `acquisitions.sync` only reads and keeps running. `writes_paused_by` is
-- the admin's username as text (survives the admin's removal).

ALTER TABLE service_instance ADD COLUMN writes_paused_at TEXT;
ALTER TABLE service_instance ADD COLUMN writes_paused_by TEXT
    CHECK (writes_paused_by IS NULL OR writes_paused_at IS NOT NULL);
