-- 0003: dedup poison — race-proof dedup + asset idempotency. Forward-only; never edit.
--
-- clips_sha256_active_uniq closes the concurrent-upload dedup race: two
-- identical bytes racing into clips both pass the SELECT dedup check, so the
-- unique index is the backstop (upload maps 23505 to 409). Failed clips stay
-- out so a poisoned/rejected hash can be re-uploaded.
CREATE UNIQUE INDEX clips_sha256_active_uniq
    ON clips (sha256_hash)
    WHERE deleted_at IS NULL AND sha256_hash IS NOT NULL AND status <> 'failed';

-- One asset per (clip, kind, rendition); transcode re-runs become no-ops on
-- the row level instead of tripping over duplicate rows.
CREATE UNIQUE INDEX media_assets_clip_kind_rendition_uniq
    ON media_assets (clip_id, kind, rendition);
