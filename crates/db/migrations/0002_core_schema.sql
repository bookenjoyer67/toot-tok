-- 0002: core schema per ARCHITECTURE.md section 3. Forward-only; never edit.

-- ---------------------------------------------------------------- actors
CREATE TABLE actors (
    id                          BIGSERIAL PRIMARY KEY,
    username                    CITEXT NOT NULL,
    domain                      TEXT,                       -- NULL => local actor
    actor_type                  TEXT NOT NULL CHECK (actor_type IN ('person', 'application', 'service')),
    public_key_pem              TEXT NOT NULL,
    private_key_pem             TEXT,                       -- NULL for remote actors
    inbox_url                   TEXT NOT NULL,
    shared_inbox_url            TEXT,
    outbox_url                  TEXT NOT NULL,
    followers_url               TEXT NOT NULL,
    display_name                TEXT,
    summary                     TEXT,
    avatar_path                 TEXT,
    header_path                 TEXT,
    manually_approves_followers BOOLEAN NOT NULL DEFAULT FALSE,
    is_locked                   BOOLEAN NOT NULL DEFAULT FALSE,
    suspended_at                TIMESTAMPTZ,
    deleted_at                  TIMESTAMPTZ,                -- self-deletion tombstone
    ap_id                       TEXT NOT NULL UNIQUE,
    created_at                  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Signup race guard: one local username per case-insensitive form.
CREATE UNIQUE INDEX actors_username_local_unique
    ON actors (username)
    WHERE domain IS NULL;

-- ---------------------------------------------------------------- users
CREATE TABLE users (
    id                   BIGSERIAL PRIMARY KEY,
    actor_id             BIGINT NOT NULL UNIQUE REFERENCES actors(id) ON DELETE CASCADE,
    email                CITEXT UNIQUE,
    email_verified_at    TIMESTAMPTZ,
    password_hash        TEXT NOT NULL,                    -- argon2id
    totp_secret          TEXT,
    totp_recovery_codes  JSONB,
    is_admin             BOOLEAN NOT NULL DEFAULT FALSE,
    deleted_at           TIMESTAMPTZ,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------- clips
CREATE TABLE clips (
    id                BIGSERIAL PRIMARY KEY,
    actor_id          BIGINT NOT NULL REFERENCES actors(id) ON DELETE RESTRICT,
    ap_id             TEXT NOT NULL UNIQUE,
    origin            TEXT NOT NULL CHECK (origin IN ('local', 'remote')),
    caption_html      TEXT,
    visibility        TEXT NOT NULL DEFAULT 'public' CHECK (visibility IN ('public', 'unlisted', 'followers')),
    status            TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'processing', 'ready', 'failed', 'deleted')),
    duration_s        DOUBLE PRECISION,
    sha256_hash       TEXT,                                -- local uploads only (dedup)
    width             INTEGER,
    height            INTEGER,
    size_bytes        BIGINT,
    remote_media_url  TEXT,                                -- remote: hot-link/cache, never transcode
    remote_poster_url TEXT,
    is_sensitive      BOOLEAN NOT NULL DEFAULT FALSE,
    cw_text           TEXT,
    comments_disabled BOOLEAN NOT NULL DEFAULT FALSE,
    downloads_allowed BOOLEAN NOT NULL DEFAULT TRUE,
    like_count        BIGINT NOT NULL DEFAULT 0,
    comment_count     BIGINT NOT NULL DEFAULT 0,
    share_count       BIGINT NOT NULL DEFAULT 0,
    view_count        BIGINT NOT NULL DEFAULT 0,
    published_at      TIMESTAMPTZ,
    deleted_at        TIMESTAMPTZ,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX clips_actor_id_idx ON clips (actor_id);
CREATE INDEX clips_sha256_hash_idx ON clips (sha256_hash) WHERE sha256_hash IS NOT NULL;

-- ---------------------------------------------------------------- media_assets
CREATE TABLE media_assets (
    id           BIGSERIAL PRIMARY KEY,
    clip_id      BIGINT NOT NULL REFERENCES clips(id) ON DELETE CASCADE,  -- local clips only
    -- 'video_hls' and 'audio' kinds are DESIGNED-FOR-LATER: v1 produces
    -- 720p/480p mp4, poster, and captions only.
    kind         TEXT NOT NULL CHECK (kind IN ('video_hls', 'video_mp4', 'poster', 'preview', 'captions', 'audio')),
    -- '1080' and 'audio' renditions are DESIGNED-FOR-LATER: v1 transcodes to
    -- 720p/480p mp4 only.
    rendition    TEXT NOT NULL DEFAULT 'orig' CHECK (rendition IN ('1080', '720', '480', 'audio', 'orig')),
    lang         TEXT,                                   -- WebVTT language for captions
    path         TEXT NOT NULL,
    mime         TEXT NOT NULL,
    size_bytes   BIGINT,
    bitrate_kbps INTEGER,
    codec        TEXT,
    ready_at     TIMESTAMPTZ
);

CREATE INDEX media_assets_clip_id_idx ON media_assets (clip_id);

-- ---------------------------------------------------------------- comments
CREATE TABLE comments (
    id                BIGSERIAL PRIMARY KEY,
    clip_id           BIGINT NOT NULL REFERENCES clips(id) ON DELETE CASCADE,
    actor_id          BIGINT NOT NULL REFERENCES actors(id) ON DELETE CASCADE,
    parent_comment_id BIGINT REFERENCES comments(id) ON DELETE CASCADE,
    ap_id             TEXT NOT NULL UNIQUE,
    body_html         TEXT NOT NULL,
    like_count        BIGINT NOT NULL DEFAULT 0,
    deleted_at        TIMESTAMPTZ,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX comments_clip_id_idx ON comments (clip_id);
CREATE INDEX comments_actor_id_idx ON comments (actor_id);
CREATE INDEX comments_parent_comment_id_idx ON comments (parent_comment_id);

-- ---------------------------------------------------------------- likes
CREATE TABLE likes (
    clip_id        BIGINT NOT NULL REFERENCES clips(id) ON DELETE CASCADE,
    actor_id       BIGINT NOT NULL REFERENCES actors(id) ON DELETE CASCADE,
    ap_activity_id TEXT UNIQUE,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (clip_id, actor_id)
);

CREATE INDEX likes_actor_id_idx ON likes (actor_id);

-- ---------------------------------------------------------------- comment_likes
CREATE TABLE comment_likes (
    comment_id     BIGINT NOT NULL REFERENCES comments(id) ON DELETE CASCADE,
    actor_id       BIGINT NOT NULL REFERENCES actors(id) ON DELETE CASCADE,
    ap_activity_id TEXT UNIQUE,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (comment_id, actor_id)
);

CREATE INDEX comment_likes_actor_id_idx ON comment_likes (actor_id);

-- ---------------------------------------------------------------- announces
CREATE TABLE announces (
    clip_id        BIGINT NOT NULL REFERENCES clips(id) ON DELETE CASCADE,
    actor_id       BIGINT NOT NULL REFERENCES actors(id) ON DELETE CASCADE,
    ap_activity_id TEXT UNIQUE,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (clip_id, actor_id)
);

CREATE INDEX announces_actor_id_idx ON announces (actor_id);

-- ---------------------------------------------------------------- follows
CREATE TABLE follows (
    follower_actor_id BIGINT NOT NULL REFERENCES actors(id) ON DELETE CASCADE,
    target_actor_id   BIGINT NOT NULL REFERENCES actors(id) ON DELETE CASCADE,
    ap_activity_id    TEXT,
    state             TEXT NOT NULL DEFAULT 'requested' CHECK (state IN ('requested', 'accepted', 'rejected')),
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (follower_actor_id, target_actor_id)
);

CREATE INDEX follows_target_actor_id_idx ON follows (target_actor_id);

CREATE OR REPLACE FUNCTION touch_updated_at() RETURNS trigger AS $$
BEGIN
    NEW.updated_at = now();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER follows_touch_updated_at
    BEFORE UPDATE ON follows
    FOR EACH ROW EXECUTE FUNCTION touch_updated_at();

-- ---------------------------------------------------------------- blocks
CREATE TABLE blocks (
    actor_id        BIGINT NOT NULL REFERENCES actors(id) ON DELETE CASCADE,
    target_actor_id BIGINT NOT NULL REFERENCES actors(id) ON DELETE CASCADE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (actor_id, target_actor_id)
);

CREATE INDEX blocks_target_actor_id_idx ON blocks (target_actor_id);

-- ---------------------------------------------------------------- domain_blocks
CREATE TABLE domain_blocks (
    domain      TEXT PRIMARY KEY,
    obfuscate   BOOLEAN NOT NULL DEFAULT FALSE,
    public_note TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------- reports
CREATE TABLE reports (
    id                BIGSERIAL PRIMARY KEY,
    reporter_actor_id BIGINT REFERENCES actors(id) ON DELETE SET NULL,   -- local only
    target_type       TEXT NOT NULL CHECK (target_type IN ('clip', 'comment', 'actor')),
    target_id         BIGINT NOT NULL,                                    -- polymorphic, no FK
    category          TEXT,
    body              TEXT NOT NULL DEFAULT '',
    state             TEXT NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'resolved', 'rejected')),
    assigned_to       BIGINT REFERENCES users(id) ON DELETE SET NULL,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at       TIMESTAMPTZ
);

CREATE INDEX reports_reporter_actor_id_idx ON reports (reporter_actor_id);

-- ---------------------------------------------------------------- hashtags
CREATE TABLE hashtags (
    id  BIGSERIAL PRIMARY KEY,
    tag CITEXT NOT NULL UNIQUE
);

CREATE TABLE clip_hashtags (
    clip_id    BIGINT NOT NULL REFERENCES clips(id) ON DELETE CASCADE,
    hashtag_id BIGINT NOT NULL REFERENCES hashtags(id) ON DELETE CASCADE,
    PRIMARY KEY (clip_id, hashtag_id)
);

CREATE INDEX clip_hashtags_hashtag_id_idx ON clip_hashtags (hashtag_id);

-- ---------------------------------------------------------------- notifications
CREATE TABLE notifications (
    id              BIGSERIAL PRIMARY KEY,
    actor_id        BIGINT NOT NULL REFERENCES actors(id) ON DELETE CASCADE,   -- recipient
    kind            TEXT NOT NULL CHECK (kind IN ('follow', 'like', 'comment', 'mention', 'boost')),
    source_actor_id BIGINT NOT NULL REFERENCES actors(id) ON DELETE CASCADE,
    clip_id         BIGINT REFERENCES clips(id) ON DELETE SET NULL,
    read_at         TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX notifications_actor_id_idx ON notifications (actor_id);
CREATE INDEX notifications_clip_id_idx ON notifications (clip_id);

-- ---------------------------------------------------------------- user_prefs
CREATE TABLE user_prefs (
    user_id            BIGINT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    notification_kinds JSONB NOT NULL DEFAULT '[]'::jsonb,
    autoplay           BOOLEAN NOT NULL DEFAULT TRUE,
    reduced_data       BOOLEAN NOT NULL DEFAULT FALSE,
    theme              TEXT NOT NULL DEFAULT 'system'
);

-- ---------------------------------------------------------------- activities
CREATE TABLE activities (
    id           BIGSERIAL PRIMARY KEY,
    activity_id  TEXT NOT NULL UNIQUE,                   -- idempotency gate
    direction    TEXT NOT NULL CHECK (direction IN ('inbound', 'outbound')),
    actor_ap_id  TEXT NOT NULL,
    object_ap_id TEXT,
    raw          JSONB NOT NULL,
    received_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    processed_at TIMESTAMPTZ
);

-- ---------------------------------------------------------------- tombstones
CREATE TABLE tombstones (
    ap_id      TEXT PRIMARY KEY,
    type       TEXT NOT NULL,
    deleted_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------- jobs
CREATE TABLE jobs (
    id           BIGSERIAL PRIMARY KEY,
    kind         TEXT NOT NULL,
    payload      JSONB NOT NULL DEFAULT '{}'::jsonb,
    run_after    TIMESTAMPTZ NOT NULL DEFAULT now(),
    attempts     INTEGER NOT NULL DEFAULT 0,
    max_attempts INTEGER NOT NULL DEFAULT 5,
    last_error   TEXT,
    state        TEXT NOT NULL DEFAULT 'queued' CHECK (state IN ('queued', 'running', 'done', 'dead')),
    locked_by    TEXT,
    locked_at    TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX jobs_state_run_after_idx ON jobs (state, run_after);
CREATE INDEX jobs_state_locked_at_idx ON jobs (state, locked_at);

-- ---------------------------------------------------------------- oauth_clients
CREATE TABLE oauth_clients (
    id                 BIGSERIAL PRIMARY KEY,
    client_id          TEXT NOT NULL UNIQUE,
    client_secret_hash TEXT NOT NULL,
    name               TEXT NOT NULL,
    redirect_uris      JSONB NOT NULL DEFAULT '[]'::jsonb,
    scopes             TEXT NOT NULL DEFAULT '',
    created_by_admin   BOOLEAN NOT NULL DEFAULT FALSE,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------- oauth_tokens
CREATE TABLE oauth_tokens (
    id                 BIGSERIAL PRIMARY KEY,
    client_id          BIGINT NOT NULL REFERENCES oauth_clients(id) ON DELETE CASCADE,
    user_id            BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash         TEXT NOT NULL UNIQUE,
    refresh_token_hash TEXT UNIQUE,
    scopes             TEXT NOT NULL DEFAULT '',
    expires_at         TIMESTAMPTZ NOT NULL,
    revoked_at         TIMESTAMPTZ,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX oauth_tokens_client_id_idx ON oauth_tokens (client_id);
CREATE INDEX oauth_tokens_user_id_idx ON oauth_tokens (user_id);

-- ---------------------------------------------------------------- oauth_device_codes
CREATE TABLE oauth_device_codes (
    device_user_code TEXT PRIMARY KEY,
    client_id        BIGINT NOT NULL REFERENCES oauth_clients(id) ON DELETE CASCADE,
    user_code        TEXT NOT NULL,
    scopes           TEXT NOT NULL DEFAULT '',
    expires_at       TIMESTAMPTZ NOT NULL,
    approved_by      BIGINT REFERENCES users(id) ON DELETE SET NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX oauth_device_codes_client_id_idx ON oauth_device_codes (client_id);
CREATE UNIQUE INDEX oauth_device_codes_user_code_idx ON oauth_device_codes (user_code);

-- ---------------------------------------------------------------- password_reset_tokens
CREATE TABLE password_reset_tokens (
    user_id    BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    expires_at TIMESTAMPTZ NOT NULL,
    used_at    TIMESTAMPTZ
);

CREATE INDEX password_reset_tokens_user_id_idx ON password_reset_tokens (user_id);

-- ---------------------------------------------------------------- sessions
CREATE TABLE sessions (
    id         TEXT PRIMARY KEY,                       -- session token hash
    user_id    BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    expires_at TIMESTAMPTZ NOT NULL,
    ip         TEXT,
    ua         TEXT
);

CREATE INDEX sessions_user_id_idx ON sessions (user_id);

-- ---------------------------------------------------------------- instances
CREATE TABLE instances (
    domain          TEXT PRIMARY KEY,
    software        TEXT,
    version         TEXT,
    inbox_url       TEXT NOT NULL,
    disabled_at     TIMESTAMPTZ,
    failure_count   INTEGER NOT NULL DEFAULT 0,
    last_success_at TIMESTAMPTZ
);

-- ---------------------------------------------------------------- settings
CREATE TABLE settings (
    key        TEXT PRIMARY KEY,
    value      JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------- audit_log
CREATE TABLE audit_log (
    id             BIGSERIAL PRIMARY KEY,
    admin_actor_id BIGINT REFERENCES actors(id) ON DELETE SET NULL,
    action         TEXT NOT NULL,
    target_type    TEXT NOT NULL,
    target_id      BIGINT,
    payload        JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX audit_log_admin_actor_id_idx ON audit_log (admin_actor_id);

-- ---------------------------------------------------------------- updated_at triggers
CREATE TRIGGER actors_touch_updated_at
    BEFORE UPDATE ON actors
    FOR EACH ROW EXECUTE FUNCTION touch_updated_at();

CREATE TRIGGER clips_touch_updated_at
    BEFORE UPDATE ON clips
    FOR EACH ROW EXECUTE FUNCTION touch_updated_at();
