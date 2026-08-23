# Architecture — federated short-video platform (name: TootTok)

Status: DRAFT v2 — incorporates adversarial review round 1 (all P1/P2/P3
addressed) + round 2 PASS with 8 polish nits (all fixed). Review log lives
in docs/research/ — this dir is not yet a git repo; init lands Phase 1.

## 1. Stack

| Layer | Choice | Why |
|---|---|---|
| Backend | Rust + Axum 0.8 + Tokio | team default; type safety; single static binary deploy story |
| Database | PostgreSQL 16+ | owner decision; JSONB for raw AP objects; what Mastodon/PeerTube admins already run |
| ORM/SQL | SQLx 0.8.x ≥ 0.8.6 pinned for v1 (runtime queries only) | RUSTSEC-2024-0363 fixed in 0.8.1; no open advisories ≥0.8.6 (verified); avoids DATABASE_URL coupling |
| Federation | `activitypub_federation` pinned EXACT `=0.7.0-beta.11` | CVE-2026-33693 SSRF (0.0.0.0 bypass) fixed in beta.9; stable 0.6.x vulnerable. Beta-line risk ACCEPTED as Phase 0 decision D6; re-audit advisory state before v1 freeze (Phase 8) |
| Media | ffmpeg/ffprobe subprocesses in hardened worker | PeerTube-proven shape; sandboxing requirements in §5 |
| Job queue | Postgres-backed SKIP LOCKED + stale-lock reaper + tick scheduler | zero extra infra; crash-safe; recurring jobs (GC, pruning) via tick |
| Frontend | SvelteKit 5 static adapter **with SPA fallback**, served by Axum; PWA | one codebase; deep links must survive refresh — adapter `fallback` + axum catch-all required (§7) |
| Auth | Argon2id + server-side sessions; OAuth2 (auth-code+PKCE, device flow) w/ refresh tokens; PAT interim | open API goal |
| Email | Optional SMTP module: verification + password reset when configured; approval-mode signup when not | Phase 0 decision D5 — spam/recovery stance is explicit either way |
| Search | Postgres native: `citext` hashtags + pg_trgm trigram index on captions | no external search service for small hosts |
| Storage | Local filesystem default; S3-compatible behind storage trait | small host first |
| Edge | Caddy in compose (auto-TLS); systemd hosts document certbot/Caddy; **TLS mandatory** — WebFinger/AP deliveries assume HTTPS | federation cannot work cleartext; non-technical admins get working certs out of the box |
| License | AGPLv3 | owner requirement; matches fediverse norms and the federation crate |

## 2. Repository layout

```
toot-tok/
├── Cargo.toml               # workspace
├── crates/
│   ├── db/                  # sqlx models + migrations (no axum deps)
│   ├── federation/          # AP types, inbox handlers, delivery, signing, egress guard
│   ├── media/               # upload validation, probe, transcode ladder, posters, GC
│   └── server/              # axum binary: routes, middleware, api, spa serving
├── web/                     # SvelteKit app (adapter-static, fallback: index.html)
├── docs/                    # VISION.md, ARCHITECTURE.md, research/, api/
├── deploy/
│   ├── docker-compose.yml   # caddy + app + postgres (+ minio commented)
│   ├── Caddyfile            # auto-TLS reverse proxy, /.well-known passthrough
│   └── systemd/toottok.service # with ProtectSystem/PrivateTmp/NoNewPrivileges
└── tests/                   # federation smoke tests (scripted, cross-instance)
```

Crate rule: `server` depends on all; `media` and `federation` depend on
`db`; `db` depends on nothing internal. Each compiles alone.
Web assets: bundled via rust-embed into the release binary (true single
binary; Node build step runs in release CI before cargo).

## 3. Data model (core tables)

```
actors            id, username citext, domain(NULL=local), actor_type(person|application|service),
                  public_key_pem, private_key_pem(NULL remote), inbox_url, shared_inbox_url,
                  outbox_url, followers_url, display_name, summary, avatar_path, header_path,
                  manually_approves_followers bool, is_locked bool,
                  suspended_at NULL, deleted_at NULL,           -- suspension vs self-deletion
                  ap_id UNIQUE NOT NULL, created_at, updated_at
                  -- PARTIAL UNIQUE INDEX ON (username) WHERE domain IS NULL  [signup race guard]
users             id BIGSERIAL PK, actor_id FK UNIQUE NOT NULL,
                  email citext UNIQUE NULLABLE, email_verified_at NULL,
                  password_hash(argon2id), totp_secret NULL, totp_recovery_codes jsonb NULL,
                  is_admin bool, deleted_at NULL, created_at
clips             id, actor_id FK, ap_id TEXT NOT NULL UNIQUE,      -- local rows get canonical URI too
                  origin TEXT NOT NULL CHECK(origin IN ('local','remote')),
                  caption_html(sanitized), visibility(public|unlisted|followers),
                  status(pending|processing|ready|failed|deleted),  -- status meaningful for LOCAL only;
                                                                    -- REMOTE rows insert directly as ready
                  duration_s, sha256_hash INDEX(dedup, local uploads only), width, height, size_bytes,
                  remote_media_url NULL, remote_poster_url NULL,    -- remote content: we hot-link/cache, never transcode
                  is_sensitive bool, cw_text NULL, comments_disabled bool, downloads_allowed bool,
                  like_count, comment_count, share_count, view_count,
                  published_at, deleted_at(tombstone), created_at, updated_at
media_assets      id, clip_id FK (LOCAL clips only), kind(video_hls|video_mp4|poster|preview|captions|audio),
                  -- video_hls/audio/1080 are DESIGNED-FOR-LATER: v1 ladder produces 720p/480p
                  -- mp4 + poster + captions only (see §5); enums kept wide on purpose.
                  rendition(1080|720|480|audio|orig), lang NULL(for captions/WebVTT),
                  path/storage_key, mime, size_bytes, bitrate_kbps, codec, ready_at
comments          id, clip_id FK, actor_id FK, parent_comment_id NULL,
                  ap_id TEXT NOT NULL UNIQUE,                       -- local comments federate too (§4)
                  body_html(sanitized), like_count, deleted_at, created_at
likes             clip_id FK, actor_id FK, ap_activity_id UNIQUE NULL, created_at, PK(clip_id,actor_id)
comment_likes     comment_id, actor_id, ap_activity_id UNIQUE NULL, PK pair
announces         clip_id FK, actor_id FK, ap_activity_id UNIQUE NULL, created_at,
                  PK(clip_id, actor_id)                             -- boosts; Undo(Announce) deletes row
follows           follower_actor_id, target_actor_id, ap_activity_id NULL,
                  state(requested|accepted|rejected), created_at, updated_at,
                  UNIQUE(follower_actor_id, target_actor_id)
blocks            actor_id, target_actor_id, created_at, PK pair
domain_blocks     domain PK, obfuscate bool, public_note, created_at     -- admin-level
reports           id, reporter_actor_id NULL(local only), target_type(clip|comment|actor),
                  target_id, category, body, state(open|resolved|rejected), assigned_to, created_at, resolved_at
hashtags          id, tag citext UNIQUE; clip_hashtags(clip_id, hashtag_id)
notifications     id, actor_id(recipient), kind(follow|like|comment|mention|boost),
                  source_actor_id, clip_id NULL, read_at, created_at
user_prefs        user_id PK/FK, notification_kinds jsonb,          -- per-kind opt-out (VISION promise)
                  autoplay bool default true, reduced_data bool, theme
activities        id, activity_id TEXT UNIQUE, direction(inbound|outbound), actor_ap_id,
                  object_ap_id NULL, raw jsonb NOT NULL, received_at, processed_at NULL
                  -- IDEMPOTENCY GATE: every inbound activity checked by activity_id before processing;
                  -- redelivery = skip. Out-of-order policy: Create-before-actor-fetch triggers lazy
                  -- actor fetch; Delete-before-Create stores tombstone which wins over later Create.
tombstones        ap_id PK, type, deleted_at
jobs              id, kind, payload jsonb, run_after, attempts, max_attempts, last_error,
                  state(queued|running|done|dead), locked_by, locked_at,
                  INDEX(state, run_after), created_at
                  -- REAPER: running jobs with locked_at older than N×timeout re-queued automatically
oauth_clients     id, client_id UNIQUE, client_secret_hash, name, redirect_uris jsonb,
                  scopes, created_by_admin bool, created_at
oauth_tokens      id, client_id FK, user_id FK, token_hash UNIQUE, refresh_token_hash UNIQUE NULL,
                  scopes, expires_at, revoked_at NULL, created_at
oauth_device_codes device_user_code UNIQUE, client_id FK, user_code, scopes,
                  expires_at, approved_by NULL, created_at               -- device flow pending state
password_reset_tokens user_id FK, token_hash UNIQUE, expires_at, used_at NULL
sessions          id(hash), user_id, expires_at, ip, ua
instances         domain PK, software, version, inbox_url, disabled_at, failure_count, last_success_at
settings          key PK, value jsonb, updated_at                    -- runtime admin toggles:
                  -- registration_mode(open|approval|invite), federation_mode(open|allowlist),
                  -- upload_size_cap_mb, clip_max_seconds, per_user_storage_quota_mb, ...
audit_log         id, admin_actor_id FK, action, target_type, target_id, payload jsonb, created_at
media_garbage     handled by worker maintenance tick (not a jobs row):
                  media_gc sweep every 60s — deletes files w/o referencing
                  asset rows ONLY after a 1h mtime grace period (in-flight
                  upload protection), removes asset rows whose file vanished,
                  sweeps assets of deleted clips. Quota enforced at UPLOAD
                  time (per_user_storage_quota_mb, 0=unlimited).
```

Rules carried from teacher codebases + review:
- Every federated object carries canonical `ap_id`; local rows included.
- Denormalized counters on `clips`/`comments`, reconciled lazily.
- Deletes = tombstones everywhere; account deletion fans out Delete for
  actor + all their objects, purges personal data rows (email, hashes,
  keys), retains tombstones (moderation + Undo hygiene). Retention policy
  documented in moderation guide.
- Raw inbound/outbound activities stored in `activities` BEFORE processing.
- `sha256` dedup applies to local uploads only.
- Remote clips: never transcoded; poster/media URLs cached per admin toggle.

## 4. Federation design

Goal per owner: "however wide ActivityPub can reach." Be boringly compatible.

### Interop shape (VERIFIED against Loops source + validator)
Publish clips as `Note` whose attachment is a `Document` with
`mediaType: video/mp4`. Mastodon/Pixelfed render the player INLINE;
text-only clients show caption+link. Our mp4 routes MUST return
`Content-Type: video/mp4` + `Accept-Ranges: bytes` (players depend on both).
HTML permalinks carry `<link rel="alternate" type="application/activity+json">`.
Inbound validation mirrors Loops' rules: `Document`|`Video`, mp4 mediaType.

### Actors
- Local users: `Person`, RSA-2048+ keypair at signup.
- Instance actor: `Application` at `/ap/actor`; also discoverable via
  WebFinger `acct:domain@domain` (some servers look it up).
- Remote actors cached in `actors`; refreshed on staleness; lazy fetch on
  first sight of unknown actor in any activity.

### Endpoints (server-to-server)
- `GET /.well-known/webfinger` (RFC 7033) — users AND instance actor
- `GET /ap/actor`, `POST /ap/inbox` (shared inbox, primary)
- `GET /users/{username}` (activity+json negotiation)
- `GET /users/{u}/inbox|outbox|followers|following` (paged collections)
- `GET /clips/{id}` (`Note`), `GET /comments/{id}` (`Note` w/ inReplyTo)
- NodeInfo: advertise BOTH 2.0 and 2.1 links in `/.well-known/nodeinfo`.
  (2.2 exists in the spec now; 2.0+2.1 covers current crawlers — fine v1.)

### Activities v1
Outbound: Create(Note), Update(Note/Person), Delete(Tombstone),
Follow/Undo(Follow), Like/Undo(Like), Announce/Undo(Announce),
Accept/Reject(Follow), Block, Delete(Person) [account deletion].
Inbound: same set + Move (accept-and-log v1).
Comments = Create(Note) inReplyTo; **delivery targets for a comment =
clip author's inbox + mentioned actors' inboxes + thread participants
per reply chain** (else Mastodon replayers never receive replies).

### Delivery
- Via crate: signed POSTs, shared-inbox fan-out, exponential backoff,
  dead-letter after N attempts (job state=dead, admin-visible).
- Inbound pipeline order (strict):
  1. HTTP signature verify (draft-cavage rsa-sha256, Date skew ≤12h)
  2. activity_id idempotency check against `activities`
  3. tombstone check (deleted object → swallow Create silently)
  4. store raw in `activities`
  5. process → mark processed_at
- Account deletion fans out Delete(actor) + Delete(each object).

### Fetch-side security (belt AND suspenders)
- Crate pinned `=0.7.0-beta.11` (CVE-2026-33693 fixed line).
- OWN egress guard with DNS-rebinding defense: resolve once, VALIDATE,
  then CONNECT TO THE PINNED IP (custom reqwest connector / SocketAddr) —
  never re-resolve at connect time (that TOCTOU is the documented bypass
  class from both upstream CVEs). Reject loopback/private/link-local/
  CGNAT/multicast/reserved ranges; TLS required; hostname verified
  against original URL at connect.
- Optional strict authorized-fetch mode exists BUT is documented as
  breaking media fetch for non-signing servers; default OFF, media URLs
  for non-public clips use signed expiring tokens instead (§5).

## 5. Media pipeline

```
POST /api/v1/clips/upload (multipart)
  → validate: magic bytes (mp4/webm/mov), size cap (admin setting)
  → sha256 dedup check
  → store original; clips(status=pending); enqueue probe
probe:      ffprobe -json → duration/w/h/audio
              → REJECT PATH: duration > cap OR undecodable ⇒ status=failed,
                enqueue cleanup (delete file), notify uploader WHY
transcode:  ffmpeg ladder (only ≤ source res): 720p H.264+AAC mp4, 480p mp4,
            EVERY mp4 output gets -movflags +faststart (moov front-loaded —
            range-request playback depends on it; asserted in CI fixtures)
            — v1.1 amendment: sources <480p get ONE upscaled 480p rung so
            every served rung is faststart h264/aac; orig stays untouched.
poster:     frame @25%; preview webm later
captions:   WebVTT upload accepted at upload/edit; stored as asset kind=captions
finalize:   assets stamped, status=ready → enqueue federation Create
```

Worker hardening (attacker-supplied input!):
- systemd unit ships ProtectSystem=strict, PrivateTmp, NoNewPrivileges,
  RestrictAddressFamilies; compose variant uses dedicated container.
- Per-job timeout + memory/thread limits via ffmpeg flags; worker kills
  stragglers and marks job dead.
- Probe-before-transcode ordering is MANDATORY (duration unknowable from
  magic bytes alone).

Serving:
- Range-request capable routes; immutable cache headers keyed by uuid.
- Public clips: plain URLs. Followers-only/unlisted: signed expiring
  media URLs (HMAC, short TTL) — works with players via redirect.
- Storage trait LocalStore|S3Store; media_gc tick job sweeps orphans,
  failed-upload remnants, deleted-clip assets; enforces per-user quota
  (admin setting) at upload time.

## 6. API surface

- REST `/api/v1`, JSON, OpenAPI (utoipa) at `/api/v1/openapi.json` + docs page.
- Resources: accounts, clips, feeds(following chronological | discover
  opt-in ranked), tags, comments, likes, announces, follows, notifications,
  reports, instance metadata, settings(admin).
- Cursor pagination; RFC 9457 problem+json errors.
- Rate limits: anon per-IP bucket, authed per-account, stricter per-domain
  on inbox POST, aggressive per-IP on signup.
- Search: `@user@domain` resolution via actors table; hashtags citext;
  caption substring via pg_trgm (configurable threshold).
- CORS: v1 = same-origin only, no CORS headers (official web app is the only
  browser client); third-party native clients use OAuth + no CORS need.
  Revisit if/when an embeddable JS SDK ships.
- Versioning additive within v1; breaking → /api/v2.

## 7. Frontend (SvelteKit, PWA)

- adapter-static WITH `fallback: 'index.html'` (SPA mode requires it);
  axum catch-all serves the shell for all non-API/non-media routes.
  Acceptance test: hard-refresh on /clip/{id} and @user MUST render.
- Routes: /(Following|Discover swipe feed), /upload, /clip/{id}, @user grid,
  /notifications, /settings, /search, /admin.
- Player: muted-autoplay, tap-for-sound, swipe-snap, preload NEXT METADATA
  ONLY, reduced-motion honored, keyboard nav, captions rendered from WebVTT.
- Onboarding reality (review finding): fresh servers have empty feeds.
  Ship curated starter-pack file (bundled JSON of suggested fediverse
  accounts, admin-editable) + make follow-suggestions skippable with
  Discover fallback; Discover seeds from relay/remote content when local
  is thin.
- Admin screens: dashboard (jobs incl. dead-letter, instances, reports),
  domain blocks + blocklist import, settings editor (registration mode,
  federation mode, caps, quotas).
- Lighthouse ≥90 perf+a11y gate in CI.
- PWA: manifest, offline shell, install prompt; push later (VAPID).

## 8. Security posture

- Argon2id baseline params; sessions httpOnly+Secure+SameSite=Lax;
  CSRF = SameSite + custom `X-Requested-With`/`X-CSRF` header check
  (stronger than bare double-submit for SPA+cookie; double-submit kept
  only as legacy fallback).
- Signup spam control per D5: open+email-verify (default when SMTP set),
  approval queue (when not), per-IP rate limit always, optional captcha hook.
- Password reset via emailed token (hashed, single-use, TTL).
- TOTP 2FA enrollment + recovery codes (Phase 8).
- Strict CSP (self only); sanitized HTML allowlist (no links v1);
  upload magic-byte sniffing + probe reject path; egress guard §4;
  signature check on EVERY s2s request; audit_log for all admin actions.

## 9. Deployment & ops

- `toottok serve --config toottok.toml`; **binary + Postgres + media dir +
  ffmpeg/ffprobe installed** (VISION updated to match). Compose includes
  Caddy for automatic TLS — the blessed non-technical path.
- systemd unit (hardened per §5) + certbot/Caddy doc for bare installs.
- Backups: pg_dump + media rsync/S3 lifecycle; RESTORE DRILL is part of
  Phase 8 verification (restore onto fresh VPS actually tested).
- Observability: tracing json logs, /healthz, /metrics (Prometheus),
  NodeInfo public directory.
- Migrations forward-only, embedded, N-1→N upgrade tested.
- Release: musl static-pie target; rust-embed'd frontend; signed artifacts;
  CHANGELOG; migration freeze at v1 RC.

## 10. Explicit non-goals v1

DMs, duets/stitches, live, playlists, P2P delivery, native apps, plugins,
custom themes, edit-history UI (AP Update handled wire-level).

## 11. Decisions (Phase 0 register)

| # | Decision | Status |
|---|---|---|
| D1 | Name | **LOCKED by owner: TootTok** |
| D2 | Client strategy | PWA-first (owner asked for more planning — revisit anytime pre-Phase 7) |
| D3 | Clip cap | 180s default, admin-adjustable setting |
| D4 | Federation default | ON + allowlist toggle (setting federation_mode) |
| D5 | Email/spam stance | SMTP module w/ verify+reset when configured; approval-mode otherwise; per-IP signup limits always |
| D6 | Beta crate risk | ACCEPTED: pin =0.7.0-beta.11, subscribe releases, re-audit at Phase 8 freeze |
