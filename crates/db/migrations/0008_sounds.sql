-- 0008: sounds — TikTok-style audio attribution. Every clip may reference a
-- named sound ("original sound — @alice", or a named track). Clips sharing a
-- sound_id group onto one sound page. No audio bytes stored here v1: the name
-- is the identity. Forward-only; never edit.
CREATE TABLE sounds (
    id               BIGSERIAL PRIMARY KEY,
    title            TEXT NOT NULL,
    author_actor_id  BIGINT REFERENCES actors(id) ON DELETE SET NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (title, author_actor_id)
);

ALTER TABLE clips ADD COLUMN sound_id BIGINT REFERENCES sounds(id) ON DELETE SET NULL;
CREATE INDEX clips_sound_id_idx ON clips (sound_id);
