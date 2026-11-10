-- Service accounts, adoption and the account plan (spec 0005 slice 3).
-- Expand only (ADR 0008): new tables and one nullable column; an older
-- binary ignores all of it.
--
-- `service_account`: Starr's record for one member on one consume instance
-- with accounts. `decision` is the admin's (`undecided`, `create`,
-- `linked`, `none`; no CHECK, so a newer release may add values and an
-- older binary reads an unknown one as undecided, ADR 0012). A member
-- without a row is undecided. `remote_id` is the account Starr owns there
-- (D4, unique per instance) with `origin` (`provisioned`: created by
-- Starr, `adopted`: linked by an admin) and `linked_username` (the name
-- there at linking or creation time). `password`: `starr` or `deferred`
-- (adopted accounts keep the member's old password until they switch, M2).
-- `decided_by` is the admin's username as text (survives the admin's
-- removal); NULL for decisions Starr made itself (new members, invitation
-- presets). Desired presence and libraries are derived, not stored. Rows
-- go with their member or instance.
CREATE TABLE service_account (
    member_id           INTEGER NOT NULL REFERENCES member (id) ON DELETE CASCADE,
    service_instance_id INTEGER NOT NULL REFERENCES service_instance (id) ON DELETE CASCADE,
    decision            TEXT NOT NULL CHECK (decision <> ''),
    origin              TEXT,
    remote_id           TEXT CHECK (remote_id IS NULL OR remote_id <> ''),
    linked_username     TEXT,
    password            TEXT,
    decided_at          TEXT NOT NULL,
    decided_by          TEXT,
    CHECK ((remote_id IS NULL) = (origin IS NULL)),
    PRIMARY KEY (member_id, service_instance_id)
) STRICT;

CREATE UNIQUE INDEX service_account_remote
    ON service_account (service_instance_id, remote_id) WHERE remote_id IS NOT NULL;
CREATE INDEX service_account_instance ON service_account (service_instance_id);

-- The last plan per instance (`accounts.reconcile`, plan only in slice 3):
-- the planner's output as JSON (`starr_core::provisioning::Plan`), its
-- counts, whether it rests on a complete listing, and a fingerprint of the
-- steps that a later approval (slice 4) names.
CREATE TABLE account_plan (
    service_instance_id INTEGER PRIMARY KEY REFERENCES service_instance (id) ON DELETE CASCADE,
    computed_at         TEXT NOT NULL,
    complete            INTEGER NOT NULL CHECK (complete IN (0, 1)),
    fingerprint         TEXT NOT NULL,
    plan                TEXT NOT NULL CHECK (json_valid(plan)),
    counts              TEXT NOT NULL CHECK (json_valid(counts))
) STRICT;

-- Invitations that name a friend's existing accounts (M3): a JSON array of
-- `{"serviceId": n, "remoteId": "…"}`; accepting links them (password
-- deferred). NULL for invitations without and for rows an older binary
-- inserts.
ALTER TABLE invitation ADD COLUMN preset_accounts TEXT
    CHECK (preset_accounts IS NULL OR (json_valid(preset_accounts) AND json_type(preset_accounts) = 'array'));
