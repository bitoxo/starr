-- Audit log (spec 0002). Entries are written in their own transaction so a
-- failed or rolled-back operation is still recorded.

CREATE TABLE audit_log (
    id         INTEGER PRIMARY KEY,
    at         TEXT NOT NULL,
    actor_kind TEXT NOT NULL CHECK (actor_kind IN ('admin', 'member', 'system', 'anonymous')),
    -- Name as text: survives renames and removal of the actor.
    actor_name TEXT,
    action     TEXT NOT NULL,
    target     TEXT,
    -- JSON object with action-specific details. Never secrets.
    details    TEXT,
    dry_run    INTEGER NOT NULL DEFAULT 0 CHECK (dry_run IN (0, 1))
) STRICT;

CREATE INDEX audit_log_at ON audit_log (at);
