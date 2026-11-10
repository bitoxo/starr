-- Exactly one active worker (ADR 0003, split mode): the worker process that
-- holds this lease executes jobs; every other worker process waits until it
-- is released or expires. At most one row (id = 1). The holder renews
-- `expires_at` by heartbeat and deletes the row on a clean shutdown.
-- Timestamps as in `job` (UTC ISO-8601 TEXT, lexical order = time order).
CREATE TABLE worker_lease (
    id          INTEGER PRIMARY KEY CHECK (id = 1),
    owner       TEXT    NOT NULL,
    acquired_at TEXT    NOT NULL,
    expires_at  TEXT    NOT NULL
) STRICT;
