-- 0006: users get a status column for approval-mode signup (Phase 4: pending
-- until an admin approves), and password_hash becomes nullable so account
-- erasure can NULL it out (ARCHITECTURE.md §3 retention/erasure rules).
-- Forward-only; never edit.
ALTER TABLE users ADD COLUMN status TEXT NOT NULL DEFAULT 'active'
    CHECK (status IN ('active', 'pending'));

ALTER TABLE users ALTER COLUMN password_hash DROP NOT NULL;
