-- 0007: bookmarks — private per-viewer saved-clips list (TikTok "Favorites").
-- No federation: bookmarks stay local, no AP activity, no notification.
-- Forward-only; never edit.
CREATE TABLE bookmarks (
    clip_id    BIGINT NOT NULL REFERENCES clips(id) ON DELETE CASCADE,
    actor_id   BIGINT NOT NULL REFERENCES actors(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (clip_id, actor_id)
);

CREATE INDEX bookmarks_actor_id_idx ON bookmarks (actor_id);
