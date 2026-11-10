-- Navidrome accounts (spec 0006 slice 7, spec 0005 D9). Expand only (ADR
-- 0008): one defaulted column; an older binary ignores it and, not knowing
-- Navidrome's accounts, never plans or writes them.
--
-- `remote_account.locked_by_starr`: Starr disabled this account on a
-- service without a disable flag (Navidrome) by writing a random password
-- nobody knows. Navidrome still reports the account as enabled; this flag
-- is the only record that it is locked. It belongs to the account, not to
-- Starr's decision about it, so unlinking and linking again (or linking it
-- to another member) keeps it. Set right before the scramble is sent (a
-- crash after it must not leave a scrambled account Starr thinks usable)
-- and after a create that stopped with the random password in place;
-- cleared when Starr writes a member's password into the account. Always
-- 0 on services with a disable flag. A scan never changes it.
ALTER TABLE remote_account ADD COLUMN locked_by_starr INTEGER NOT NULL DEFAULT 0
    CHECK (locked_by_starr IN (0, 1));
