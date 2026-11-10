-- Initial schema. Committed migrations are immutable: add a new file instead
-- of editing this one (enforced by .claude/hooks/guard.sh and CI).

CREATE TABLE app_meta (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
) STRICT;

INSERT INTO app_meta (key, value) VALUES ('created_at', strftime('%Y-%m-%dT%H:%M:%SZ', 'now'));
