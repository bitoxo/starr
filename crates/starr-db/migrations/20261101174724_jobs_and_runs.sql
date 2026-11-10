-- Job queue and run history (ADR 0004).
--
-- `job` is the queue: a row exists only while the job is pending or running
-- and is deleted when it finishes for good. "Running" is not a stored flag but
-- a valid lease (`lease_owner`, `lease_until`, renewed by heartbeat); an
-- expired lease makes the job claimable again (lessons 14, 20).
-- History lives in `run` / `run_transition`, which outlive the job row.
--
-- Timestamps are UTC ISO-8601 TEXT with millisecond precision and a `Z`
-- suffix (`2026-11-01T12:00:00.000Z`), so lexical order is time order.

-- AUTOINCREMENT: ids of deleted jobs are never reused, so `run.job_id`
-- stays unambiguous.
CREATE TABLE job (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    kind         TEXT    NOT NULL,
    payload      TEXT    NOT NULL DEFAULT '{}' CHECK (json_valid(payload)),
    unique_key   TEXT,
    trigger      TEXT    NOT NULL CHECK (trigger IN ('schedule', 'manual')),
    run_after    TEXT    NOT NULL,
    attempts     INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    max_attempts INTEGER NOT NULL CHECK (max_attempts >= 1),
    lease_owner  TEXT,
    lease_until  TEXT,
    created_at   TEXT    NOT NULL,
    CHECK ((lease_owner IS NULL) = (lease_until IS NULL))
) STRICT;

-- Dedup: at most one pending/running job per key. Finished jobs are deleted,
-- so every row is pending or running.
CREATE UNIQUE INDEX job_unique_key ON job (unique_key) WHERE unique_key IS NOT NULL;
-- Claim query: due jobs, oldest first.
CREATE INDEX job_due ON job (run_after, id);

CREATE TABLE run (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    -- No foreign key: the job row is deleted when the job finishes.
    job_id      INTEGER NOT NULL,
    kind        TEXT    NOT NULL,
    trigger     TEXT    NOT NULL CHECK (trigger IN ('schedule', 'manual')),
    attempt     INTEGER NOT NULL CHECK (attempt >= 1),
    state       TEXT    NOT NULL CHECK (state IN ('running', 'succeeded', 'failed', 'aborted')),
    -- Live progress 0.0..=1.0, overwritten in place (lesson 19).
    progress    REAL    CHECK (progress IS NULL OR (progress >= 0.0 AND progress <= 1.0)),
    -- Structured report; a finished run without one counts as failed (lesson 28).
    report      TEXT    CHECK (report IS NULL OR json_valid(report)),
    expectation TEXT    CHECK (expectation IS NULL OR expectation IN ('met', 'unmet', 'not_applicable')),
    error       TEXT,
    started_at  TEXT    NOT NULL,
    finished_at TEXT,
    CHECK ((state = 'running') = (finished_at IS NULL))
) STRICT;

-- Listing is newest first (id order), optionally filtered by kind or state.
CREATE INDEX run_kind ON run (kind, id);
CREATE INDEX run_state ON run (state, id);
CREATE INDEX run_job ON run (job_id);
-- Housekeeping deletes by age.
CREATE INDEX run_finished_at ON run (finished_at);

CREATE TABLE run_transition (
    id         INTEGER PRIMARY KEY,
    run_id     INTEGER NOT NULL REFERENCES run (id) ON DELETE CASCADE,
    from_state TEXT    CHECK (from_state IS NULL OR from_state IN ('running', 'succeeded', 'failed', 'aborted')),
    to_state   TEXT    NOT NULL CHECK (to_state IN ('running', 'succeeded', 'failed', 'aborted')),
    note       TEXT,
    at         TEXT    NOT NULL
) STRICT;

CREATE INDEX run_transition_run ON run_transition (run_id, id);
