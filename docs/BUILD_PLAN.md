# TootTok Build Plan (master)

> **For Hermes:** execute via subagent-driven-development; fresh subagent per
> phase, spec review after each. Verify every claim against primary sources.
>
> **Goal:** federated short-video platform — TikTok feel, Mastodon values,
> Rust/Axum backend + SvelteKit PWA, AGPLv3. Name: **TootTok**.
>
> **Architecture:** 4-crate workspace (`db`, `media`, `federation`, `server`),
> Postgres-backed job queue w/ stale-lock reaper, ActivityPub via
> `activitypub_federation` (=0.7.0-beta.11), clips published as `Note`+video
> attachment. Full design: ARCHITECTURE.md v2 (review round 1 incorporated).

**Ground rules for every phase**

- TDD: failing test first where the phase ships logic.
- After every phase: `cargo check && cargo test` green; server phases end
  with live smoke test (background start → curl health → one write).
- Never commit without local build passing. No pushes without explicit OK.
- Migrations forward-only, additive; applied files immutable.
- External claims verified from primary source at implementation time.

---

## Phase 0 — Decisions register

| # | Decision | Default if owner silent |
|---|---|---|
| D1 | Project name | **LOCKED by owner: TootTok** |
| D2 | Client strategy | PWA-first |
| D3 | Clip length cap | 180s, admin-adjustable setting |
| D4 | Federation default | ON, allowlist toggle available |
| D5 | Email/spam stance | SMTP optional module: verify+reset when configured; approval-mode signup otherwise; per-IP signup limits always |
| D6 | Beta federation crate risk | ACCEPTED; pin exact beta; re-audit at freeze |

## Phase 1 — Workspace skeleton

1. Workspace: crates `db`, `media`, `federation`, `server`; each compiles
   alone (`cargo check -p …`).
2. Shared deps: axum 0.8.x, sqlx 0.8.x≥0.8.6 (runtime queries), tokio,
   serde, tracing, uuid, chrono, reqwest(rustls), rsa, anyhow/thiserror.
3. `server` starts axum, `/healthz` smoke-tested live.
4. AGPLv3 LICENSE, README stub, .gitignore, fmt/clippy config
   (clippy --release zero warnings is release law).
5. deploy/ skeleton: docker-compose.yml (caddy + app + postgres),
   Caddyfile, hardened systemd unit.

## Phase 2 — Database core

Full schema per ARCHITECTURE.md §3, explicitly INCLUDING:

1. Core: migrations CREATE EXTENSION IF NOT EXISTS citext + pg_trgm first.
   Then: actors (+partial UNIQUE username WHERE domain IS NULL), users,
   clips (ap_id NOT NULL UNIQUE, origin local|remote, remote_media_url),
   media_assets (kind incl. captions, lang), comments (ap_id NOT NULL
   UNIQUE), likes/comment_likes (+ap_activity_id), **announces**,
   follows (UNIQUE pair, updated_at), blocks(+created_at), domain_blocks,
   reports, hashtags(citext)+clip_hashtags, notifications, user_prefs,
   **activities (activity_id UNIQUE — idempotency)**, tombstones, jobs
   (INDEX(state, run_after)), instances, **settings(key,value jsonb)**,
   **audit_log**, oauth_clients, oauth_tokens(+refresh/revoked),
   oauth_device_codes, password_reset_tokens, sessions.
2. Runtime-query models only; integration tests against real Postgres.
3. Migration tests: fresh DB clean; N-1→N path exercised once.
4. `toottok create-admin` CLI wired to users/actors insert.

## Phase 3 — Media pipeline

1. Upload: multipart, magic-byte sniff, size cap (settings), sha256 dedup
   (local only) → status=pending → enqueue probe.
2. Probe job: ffprobe -json; **REJECT path**: over-cap duration or
   undecodable ⇒ status=failed + file cleanup + clear uploader error.
   Probe-before-transcode mandatory.
3. Transcode ladder 720p/480p H.264+AAC mp4, `-movflags +faststart` on
   EVERY mp4 output (asserted in CI fixture test: moov atom position check).
4. Poster @25%; captions(WebVTT) accepted as asset kind=captions w/ lang.
5. Finalize → status=ready → enqueue federation Create (Phase 5 hook).
6. Serving: range requests, correct Content-Type video/mp4,
   Accept-Ranges: bytes, immutable cache headers; **signed expiring URLs**
   for non-public clips.
7. Worker hardening: timeout + mem/thread caps per job; stragglers killed,
   jobs dead-lettered. systemd/compose isolation per ARCHITECTURE §5.
8. media_gc tick job: orphan sweep (files w/o rows, deleted rows' assets,
   failed-upload remnants); per-user storage quota enforced at upload
   (admin setting).
9. Tests: tiny generated fixture mp4s; full upload→ready→play cycle in CI;
   reject-path test; GC test.

## Phase 4 — Accounts & auth

1. Signup/login/logout; Argon2id; server-side sessions (httpOnly+Secure+
   SameSite=Lax); CSRF = SameSite + custom header check.
2. D5 wiring: SMTP module (lettre or hand-rolled submission) — email
   verification + password reset tokens when configured; approval-queue
   mode otherwise; per-IP signup rate limits always.
3. Actor RSA keypairs at signup; profile edit; avatar/header upload.
4. Rate-limit middleware (per-IP/per-account buckets); RFC 9457 errors.
5. Settings service (settings table): registration_mode, federation_mode,
   caps, quotas — admin API guarded by is_admin + audit_log entries.
6. Moderation basics: suspend actor, hide content.
7. **Account deletion (local half)**: delete-my-account endpoint — purge
   email/hash/keys/personal rows, tombstone actor + objects, invalidate
   sessions/tokens. Remote fan-out lands in Phase 5; end-to-end retest
   in Phase 8.

## Phase 5 — Federation core

1. Dependency: `activitypub_federation` EXACT `=0.7.0-beta.11`. At impl
   time re-verify crates.io latest beta + advisory DB; if newer patched
   line exists, take it and note delta.
2. OWN egress guard, rebinding-proof: resolve once → validate ranges →
   connect PINNED IP (custom connector); TLS required; hostname checked
   vs original URL. Unit tests with mock resolver (loopback/private/
   CGNAT/rebinding cases).
3. Endpoints: webfinger (users + instance acct:domain@domain), /ap/actor,
   POST /ap/inbox (shared), user endpoints + paged collections, clip Note,
   comment Note, NodeInfo 2.0 AND 2.1 links.
4. Inbound pipeline STRICT ORDER: sig verify → activity_id idempotency
   (activities table) → tombstone check → store raw → process → stamp.
   Out-of-order policy: unknown actor ⇒ lazy fetch; Delete-before-Create
   ⇒ tombstone wins, later Create swallowed.
5. Activities both ways: Follow/Accept/Reject/Undo, Create/Update/Delete
   Note, Like/Undo, Announce/Undo, Block, Delete(Person), Move(log).
   Inbound Create(Note)+video validated per Loops rules (Document|Video,
   mp4) → clips row origin=remote, status=ready, NEVER transcoded,
   remote_media_url cached per admin toggle.
6. Delivery targets: followers' shared inboxes; comments ALSO to clip
   author + mentioned actors + thread participants. Retries/backoff/
   dead-letter via jobs table; stale-lock reaper running.
7. Raw activity log written for every in/outbound activity.
8. Cross-instance rig: two local instances real HTTP; scripted
   follow→post→like→comment→announce→delete round-trip green.
9. Account deletion fan-out: Delete(actor)+Delete(objects) delivered;
   integration test asserts remote side tombstoned.

## Phase 6 — Client-facing REST API

1. `/api/v1`: accounts, clips CRUD, feeds (following chronological /
   discover opt-in ranked behind trait), tags, comments, likes, announces,
   follows, notifications (+user_prefs filtering), reports, instance
   metadata, settings(admin). Cursor pagination; problem+json errors.
2. OpenAPI via utoipa served + docs page generated in CI.
3. Search mechanics: actors handle @user@domain resolution; hashtags
   citext equality; caption substring via pg_trgm (threshold setting).
4. OAuth2: authorization-code+PKCE AND device flow; refresh token
   rotation + revocation; scoped PAT interim for scripts.
5. Contract tests per resource; auth-negative tests (expired, revoked,
   wrong scope).

## Phase 7 — SvelteKit web app + PWA

1. adapter-static WITH fallback:'index.html'; axum catch-all serves shell
   for non-API/non-media routes; **acceptance test: hard-refresh on
   /clip/{id} and @user renders** (deep-link regression guard).
   Release bundle embedded via rust-embed (Node build precedes cargo in CI).
2. Screens: swipe feed (Following/Discover), upload (progress, caption,
   CW, WebVTT captions field), clip permalink, profile grid,
   notifications, settings (+notification prefs), search (@user@domain,
   #tags, caption trgm), report flows, delete-account flow w/ warnings.
3. Player: muted-autoplay, tap-for-sound, swipe-snap, preload NEXT
   METADATA ONLY, reduced-motion, keyboard nav, captions overlay from VTT.
4. Onboarding: bundled starter-pack JSON (curated fediverse accounts,
   admin-editable), skippable, Discover fallback for empty servers.
5. Admin screens: dashboard (jobs incl. dead letters, instances, reports
   queue), domain blocks + blocklist import, settings editor, audit log
   viewer.
6. PWA: manifest, offline shell, install prompt (push post-v1).
7. Gates: Lighthouse ≥90 perf+a11y in CI; svelte-check clean.

## Phase 8 — Hardening & ship

1. Security pass: CSP strict-self, sanitization audit, rate-limit tuning,
   session flags, `cargo audit` zero highs AND `osv-scanner` (or explicit
   GHSA check) — CVE-2026-33693 is GHSA-only, cargo audit alone is blind
   to it. **Re-audit activitypub_federation advisories** (D6 exit check).
2. TOTP 2FA enrollment + recovery codes (+tests).
3. Observability: structured json logs, /metrics Prometheus, admin alert
   notes for dead-letter pileup + disk watermark.
4. Deploy kit: compose blessed-path test, systemd hardened unit test,
   INSTALL guide for non-technical admins (domain, DNS, Caddy, first
   admin), backup doc + **RESTORE DRILL executed on fresh VPS**.
5. Federation conformance: Mastodon + Loops test instances; assertions:
   inline playback works from Mastodon (Content-Type + range verified),
   replies arrive, deletes propagate, account deletion propagates.
6. Media GC + quota soak test; disk-watermark behavior documented.
7. Docs site: rendered API reference, self-hosting guide, moderation guide
   (incl. retention policy), upgrading guide.
8. Release engineering: musl static-pie build, signed artifacts, changelog,
   migration freeze for v1 RC, clippy --release zero warnings gate.

## Verification (definition of done, whole project)

- [ ] Fresh VPS: compose up w/ real domain + cert → register → upload →
      transcode → play < 15 min
- [ ] Scripted cross-instance smoke green (us↔us↔Mastodon↔Loops)
- [ ] Mastodon inline playback + follow/like/reply round-trips verified
- [ ] Account deletion round-trip verified (local purge + remote tombstone)
- [ ] Deep-link refresh test green on all primary routes
- [ ] Lighthouse ≥ 90 perf + a11y
- [ ] Restore-from-backup drill executed successfully on fresh VPS
- [ ] Reject-path + GC + quota tests green (disk cannot silently fill)
- [ ] `cargo clippy --release` zero warnings
- [ ] Comprehensive tests on federation handlers, media pipeline, feed,
      auth (owner law)
- [ ] Zero known unfixed high CVEs incl. federation crate re-audit
- [ ] Non-technical-admin INSTALL guide never requires reading source
