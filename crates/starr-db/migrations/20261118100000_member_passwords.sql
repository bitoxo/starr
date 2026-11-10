-- Member passwords for service accounts (spec 0005 slice 5, ADR 0010).
-- Expand only (ADR 0008): one defaulted column, one nullable column, one
-- new table; an older binary ignores all of them.
--
-- `member.password_generation` increments on every Starr password set
-- (invitation, set-password link, password change). An account that uses
-- the Starr password is due when its written generation differs.
ALTER TABLE member ADD COLUMN password_generation INTEGER NOT NULL DEFAULT 0
    CHECK (password_generation >= 0);

-- `service_account.password_generation_written`: the generation Starr last
-- wrote into the account; NULL = never written (adopted accounts with the
-- member's old password, or an account whose create did not finish).
-- `service_account.password` gains the value `pending`: the member asked
-- to switch an adopted account to the Starr password (M2); the old
-- password stays until the worker has written the new one. An older
-- binary reads `pending` as an unknown value and skips the row (the member
-- counts as undecided there, so it does nothing).
ALTER TABLE service_account ADD COLUMN password_generation_written INTEGER
    CHECK (password_generation_written IS NULL OR password_generation_written >= 0);

-- Password handoffs (M1 B): the member's plaintext password sealed with
-- age to the worker's recipient (ADR 0003), bound to the member and the
-- generation (the sealed plaintext starts with `member:<id>:password:<gen>`,
-- checked on opening). At most one per member; a newer one replaces it.
-- Deleted by the worker when written, at the latest after one hour
-- (`expires_at`). Never logged, never in run reports or audit.
CREATE TABLE password_handoff (
    member_id  INTEGER PRIMARY KEY REFERENCES member (id) ON DELETE CASCADE,
    generation INTEGER NOT NULL CHECK (generation >= 0),
    recipient  TEXT NOT NULL CHECK (recipient <> ''),
    sealed     BLOB NOT NULL,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
) STRICT;

CREATE INDEX password_handoff_expiry ON password_handoff (expires_at);
