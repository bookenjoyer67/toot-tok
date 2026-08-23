# Research notes — teachers & sources

Primary-source recon done 2026-08-22 before ARCHITECTURE.md v2.
Everything below was read from source or primary pages, not marketing.

## Reference checkouts (read-only, /tmp, disposable)

- Loops server: github.com/joinloops/loops-server → /tmp/ref-loops
  (Laravel + Vue, AGPLv3, ~418★, v1.0.0-beta.12 era)
- PeerTube: github.com/Chocobozzz/PeerTube → /tmp/ref-peertube
  (Node/TypeScript, AGPLv3)

## Loops internals — what we ported as PATTERNS (not code)

- FEDERATION.md: full activity matrix (Create/Update/Delete/Follow/Accept/
  Reject/Like/Announce/Undo/Block), instance actor `Application` at /ap/actor,
  shared inbox POST /ap/inbox, HTTP sigs rsa-sha256 ≥2048, Date skew ≤60min,
  retry 30s→exponential→max 10, domain blocks kill delivery+fetch+search.
- **Note-wrap trick**: clips published as `Note` + attachment `Document`
  mediaType video/mp4 — chosen for maximum compatibility ("fediverse is
  optimized for notes"). Their validator (app/Federation/Validators/
  NoteWithVideoAttachmentValidator.php) accepts Document|Video w/ mp4.
- Data bones (migrations): videos table = caption, tags JSON, counter cols,
  CW fields, sha512 dedup index, has_hls, can_duet/can_stitch/download flags;
  sounds table tiny (hash+path+can_reshare) = TikTok-style sound reuse;
  followers table with following_is_local flag.
- Feature sprawl observed (lesson): DMs, duets, playlists, starter kits,
  quotes, relays, curated onboarding — one dev carrying all of it. Our v1
  scope cuts exist because of this list.
- Install bar to beat: PHP8.3+/MySQL8/Redis6/ffmpeg4.5+/Node20 + composer +
  npm build + artisan key:gen + storage:link; compose runs 5 services
  (mysql, redis, app, horizon, scheduler). Our bar: compose caddy+app+pg.

## PeerTube pipeline shape (docs.joinpeertube.org/contribute/architecture)

upload → job queue → transcode (multi-res ladder, CPU-bound) → storage →
federation LAST. Remote runners can pull transcoding jobs via REST.
Redundancy via WebTorrent/magnet + CacheFile activities (we skip P2P v1).
faststart discipline on mp4 outputs is unconditional there.

## Ecosystem numbers (as-of Mar 2026, per fediverse.observer via Wikipedia)

Loops: ~31 servers, ~40.8K accounts, ~4.1K MAU. Launched Oct 2024,
ActivityPub Oct 2025, iOS App Store Jan 2026. Niche real, nearly empty.

## activitypub_federation crate (crates.io / GHSA primary checks)

- Stable line: 0.6.5 (VULNERABLE to CVE-2026-33693).
- Beta line: 0.7.0-beta.x; beta.11 current as of recon date 2026-08-22
  (crates.io publish date of beta.11 was 2026-04-24).
- CVE-2025-25194 (GHSA-7723-35v7-qcxw): SSRF, fixed earlier beta.
- CVE-2026-33693 (GHSA-q537-8fr5-cw35): 0.0.0.0 bypass of the above fix in
  v4_is_invalid(); fixed 0.7.0-beta.9. Advisory ALSO documents DNS-rebinding
  TOCTOU class → our own egress guard must pin resolved IP into connection.
- License AGPL-3.0 (matches ours). Axum-compatible (feature flag), actix
  default. Used by Lemmy in production.

## Other verified stack facts

- Axum 0.8.x stable current (0.8.9 at review). nest_service("/") broken —
  use fallback_service patterns (in-house skill refs agree).
- sqlx: only advisory ever RUSTSEC-2024-0363, fixed 0.8.1; ≥0.8.6 clean;
  0.9 exists, not needed for v1. Runtime queries only (no DATABASE_URL
  compile coupling for contributors).
- NodeInfo 2.1 spec: /.well-known/nodeinfo returns JRD link block; we serve
  BOTH 2.0 and 2.1 links (older crawlers want 2.0).
- SvelteKit adapter-static SPA mode REQUIRES fallback option — else deep
  links 404. In-house proven serving pattern: axum fallback_service shell.

## Name candidates (trademark-clean shortlist)

Kino (strong brand, minor tool collisions) / Kadr ("frame", clean) /
Vydra (cleanest, weak cinema tie). Avoid tok/tube roots. Owner pick pending.
