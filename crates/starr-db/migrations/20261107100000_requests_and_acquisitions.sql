-- Requests and acquisitions (domain model §6, spec 0003 slice 4).
--
-- request: personal. One member asks for one title (a movie or a series)
-- in one category. `state` is the aggregate of its items, derived in Rust
-- (`starr_core::requests::request_state`) and stored by the repository on
-- every change, so lists and filters read one column (lesson 16).
--
-- request_item: the units of a request: the movie itself, or single
-- seasons of a series (D8). Carries the decision. `member_id` repeats the
-- owner so the "one open request per member and unit" rule can be a unique
-- index that includes the owner (lesson 4). Approved items link to the
-- shared acquisition of their unit.
--
-- acquisition: shared, no member column. One per unit (movie or season
-- catalog item) and source instance (D10: no double acquisition). Quality
-- profile, root folder and minimum availability are copied from the
-- category's source settings when the acquisition is created, so a later
-- source switch lets it finish where it started. Progress is live state,
-- overwritten by `acquisitions.sync`; transitions of the requests it
-- serves go to request_event (lesson 19).
--
-- request_event: every change of a request's state, with time, trigger
-- (member, admin, system) and the actor's name as text (survives removal).
--
-- Removing a source instance never deletes acquisitions: they keep their
-- row without an instance (open ones as failed), so no approved item is
-- left without the acquisition that explains its state.
--
-- No foreign key reaches into `inventory`: withdrawing or deleting intent
-- never touches inventory (lesson 24).

CREATE TABLE acquisition (
    id                   INTEGER PRIMARY KEY,
    catalog_item_id      INTEGER NOT NULL REFERENCES catalog_item (id),
    -- NULL once the source instance was removed: the repository fails open
    -- acquisitions first, so requests keep a visible state, and a retry
    -- moves the acquisition to the category's current source.
    service_instance_id  INTEGER REFERENCES service_instance (id) ON DELETE SET NULL,
    -- The category whose source acquires it (availability looks at this
    -- category's consume services).
    category             TEXT NOT NULL CHECK (category IN ('movies', 'series', 'music', 'adult')),
    state                TEXT NOT NULL CHECK (state IN ('pending', 'submitted', 'downloading',
                             'partially_available', 'available', 'failed')),
    quality_profile_id   INTEGER NOT NULL,
    root_folder_path     TEXT NOT NULL CHECK (root_folder_path <> ''),
    minimum_availability TEXT CHECK (minimum_availability IS NULL
                             OR minimum_availability IN ('announced', 'inCinemas', 'released')),
    progress             REAL CHECK (progress IS NULL OR progress BETWEEN 0 AND 1),
    -- The source's own explanation of a queue warning (live state).
    source_message       TEXT,
    -- The last submission failure: taxonomy code and readable reason.
    error_code           TEXT,
    last_error           TEXT,
    -- The source's id of the title (a cache; the natural key decides).
    remote_id            TEXT,
    submit_attempts      INTEGER NOT NULL DEFAULT 0 CHECK (submit_attempts >= 0),
    -- Pending: due for (re)submission from then on.
    next_attempt_at      TEXT,
    submitted_at         TEXT,
    available_at         TEXT,
    synced_at            TEXT,
    created_at           TEXT NOT NULL,
    updated_at           TEXT NOT NULL,
    UNIQUE (catalog_item_id, service_instance_id),
    -- A failed acquisition always says why; a pending one may carry the
    -- error of the attempt it retries after.
    CHECK (state <> 'failed' OR last_error IS NOT NULL)
) STRICT;

CREATE INDEX acquisition_service_instance ON acquisition (service_instance_id);
CREATE INDEX acquisition_state ON acquisition (state, next_attempt_at);

CREATE TABLE request (
    id              INTEGER PRIMARY KEY,
    member_id       INTEGER NOT NULL REFERENCES member (id) ON DELETE CASCADE,
    -- The movie or the series.
    catalog_item_id INTEGER NOT NULL REFERENCES catalog_item (id),
    category        TEXT NOT NULL CHECK (category IN ('movies', 'series', 'music', 'adult')),
    state           TEXT NOT NULL CHECK (state IN ('pending_approval', 'approved', 'submitted',
                        'in_progress', 'partially_available', 'available', 'rejected',
                        'cancelled', 'failed')),
    reject_reason   TEXT CHECK (reject_reason IS NULL OR length(reject_reason) BETWEEN 1 AND 500),
    -- "Follow new seasons" (D8); acted on by `follows.check` (slice 7).
    follow          INTEGER NOT NULL DEFAULT 0 CHECK (follow IN (0, 1)),
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    -- Target of request_item's (request_id, member_id) key: an item's owner
    -- is always its request's owner.
    UNIQUE (id, member_id)
) STRICT;

CREATE INDEX request_member ON request (member_id, created_at);
CREATE INDEX request_state ON request (state, created_at);
CREATE INDEX request_catalog_item ON request (catalog_item_id);
CREATE INDEX request_category ON request (category);

CREATE TABLE request_item (
    id              INTEGER PRIMARY KEY,
    request_id      INTEGER NOT NULL,
    -- The request's owner, repeated for the unique index below and bound
    -- to the request by the composite foreign key.
    member_id       INTEGER NOT NULL REFERENCES member (id) ON DELETE CASCADE,
    -- The movie, or one season.
    catalog_item_id INTEGER NOT NULL REFERENCES catalog_item (id),
    season_number   INTEGER CHECK (season_number IS NULL OR season_number >= 0),
    decision        TEXT NOT NULL CHECK (decision IN ('pending_approval', 'approved',
                        'rejected', 'cancelled')),
    acquisition_id  INTEGER REFERENCES acquisition (id),
    decided_at      TEXT,
    FOREIGN KEY (request_id, member_id) REFERENCES request (id, member_id) ON DELETE CASCADE,
    CHECK (acquisition_id IS NULL OR decision = 'approved')
) STRICT;

-- One open (pending or approved) request per member and unit; other
-- members' requests for the same unit are allowed and share the
-- acquisition (lesson 4: the owner is part of the rule).
CREATE UNIQUE INDEX request_item_one_open ON request_item (member_id, catalog_item_id)
    WHERE decision IN ('pending_approval', 'approved');
CREATE INDEX request_item_request ON request_item (request_id);
CREATE INDEX request_item_catalog_item ON request_item (catalog_item_id, decision);
CREATE INDEX request_item_acquisition ON request_item (acquisition_id);

CREATE TABLE request_event (
    id         INTEGER PRIMARY KEY,
    request_id INTEGER NOT NULL REFERENCES request (id) ON DELETE CASCADE,
    at         TEXT NOT NULL,
    -- NULL when the request was created.
    from_state TEXT,
    to_state   TEXT NOT NULL,
    trigger    TEXT NOT NULL CHECK (trigger IN ('member', 'admin', 'system')),
    -- Username, or the job kind for the system.
    actor      TEXT,
    detail     TEXT
) STRICT;

CREATE INDEX request_event_request ON request_event (request_id, id);
