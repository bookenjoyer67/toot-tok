-- 0004: sessions carry a per-session CSRF token (Phase 4 auth, CSRF =
-- SameSite + custom header check per ARCHITECTURE.md §8). Forward-only; never edit.
ALTER TABLE sessions ADD COLUMN csrf_token text NOT NULL DEFAULT '';
