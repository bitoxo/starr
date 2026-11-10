-- Account writes with safe mode (spec 0005 slice 4, M8, D15).
-- Expand only (ADR 0008): nullable columns; an older binary ignores them
-- and, having no write half of `Accounts`, never writes accounts anyway.
--
-- Per consume instance with accounts:
-- - `accounts_live_since` / `accounts_live_by`: an admin switched account
--   writes on (going live confirms a complete plan at most one hour old).
--   NULL = plan only, the default on upgrade and for every new instance.
-- - `writes_paused_at` / `writes_paused_by` (existing, spec 0003) now also
--   hold account writes of a live instance.
-- - `accounts_last_live_run_at`: the last complete live run; after an hour
--   without one, the next plan that writes waits for approval (M8 A).
-- - `accounts_approved_*`: an admin's approval of exactly one plan (its
--   fingerprint) for going live, resuming, or a plan held by the
--   destructive cap or the one-hour rule. Usable for one hour; cleared by
--   the run that executes it. Admin usernames are text (survive removal).
ALTER TABLE service_instance ADD COLUMN accounts_live_since TEXT;
ALTER TABLE service_instance ADD COLUMN accounts_live_by TEXT
    CHECK (accounts_live_by IS NULL OR accounts_live_since IS NOT NULL);
ALTER TABLE service_instance ADD COLUMN accounts_last_live_run_at TEXT;
ALTER TABLE service_instance ADD COLUMN accounts_approved_fingerprint TEXT;
ALTER TABLE service_instance ADD COLUMN accounts_approved_at TEXT
    CHECK ((accounts_approved_at IS NULL) = (accounts_approved_fingerprint IS NULL));
ALTER TABLE service_instance ADD COLUMN accounts_approved_by TEXT;

-- The last failed write per member × instance ("needs you"): the error
-- taxonomy code, a short detail (redacted of credentials; never a
-- password, which no error carries) and when. Cleared by the next
-- successful write.
ALTER TABLE service_account ADD COLUMN last_error TEXT;
ALTER TABLE service_account ADD COLUMN last_error_detail TEXT;
ALTER TABLE service_account ADD COLUMN last_error_at TEXT
    CHECK ((last_error_at IS NULL) = (last_error IS NULL));
