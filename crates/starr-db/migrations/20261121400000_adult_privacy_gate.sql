-- Privacy gate of the adult category (spec 0007 slice 1, ADR 0014).
--
-- session.private_unlocked_until: the session unlocked the private (adult)
-- category until then (Q7 B); every adult activity moves it 30 minutes on.
-- NULL: locked. A rotated session starts locked (not copied).
--
-- category.statement_at / statement_by: the operator statement made when
-- adult was switched on (Q12 A): when, and the admin's username. Cleared
-- when the category is switched off, so switching it on again asks again.
-- It only counts while statement_at >= updated_at, so an older binary that
-- toggles the category (and bumps updated_at) invalidates it.
--
-- Expand only (ADR 0008): nullable columns an older binary ignores.

ALTER TABLE session ADD COLUMN private_unlocked_until TEXT;

ALTER TABLE category ADD COLUMN statement_at TEXT;
ALTER TABLE category ADD COLUMN statement_by TEXT
    CHECK (statement_by IS NULL OR statement_by <> '');
