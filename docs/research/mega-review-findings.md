# Mega-review (ox-alpha, full codebase) — findings register

Final gate before deploy. 22 findings: 5×P1, 8×P2, 9×P3.
**STATUS: ALL FIXED + verified. 79/79 tests, clippy zero, fmt clean.**

Fix-wave second-order bugs caught by orchestrator verification (pattern
holds: fixes breed bugs):
- Hex-entity decoder off-by-one (`end+3` → `end+4`) corrupted every
  apostrophe to `&#x27;;` — found via caption round-trip test.
- `block_in_place` in AP handler panicked on single-thread test runtime —
  replaced with plain `.await`.
- F9 escaping is STORE-ESCAPED stance: captions persist as escaped plain
  text (`bob&#x27;s`), Note content carries the same. Tests assert this.
- Stray `>` in plain text now survives as escaped text (only tag-closing
  `>` is markup).
- Undo(Like) parses as LENIENT Undo (raw object Value) — never 400.

## P1 (deploy blockers)
- F1 Undo(Like)/Undo(Announce) 400s forever — Mastodon unlikes break. Fix:
  lenient Undo.object → passthrough/202.
- F2 Inbox bodies unbounded in RAM pre-verification (crate reads
  usize::MAX). Fix: 1MiB cap → 413.
- F3 Local account deletion fans out nothing remote; no clip-delete route.
  Fix: Delete(actor)+Delete(Tombstone) fan-out + DELETE /clips/{id}.
- F4 XFF leftmost trust = rate-limit bypass via spoofed header. Fix:
  rightmost-untrusted walk.
- F5 Outbound Note hardcodes 720.mp4 — sub-720p sources federate 404s. Fix:
  pick largest available rendition.

## P2
F6 erasure keeps private_key_pem+avatars; suspended/deleted actors still
sign deliveries. F7/F8 deliver_create races delete/suspend (no recheck);
DB error strings reach clients. F9 strip_html passes entities through
(`&lt;img onerror…` smuggles). F10 signup/delete_me lack transactions.
F11 assets/clips metadata ignore visibility + deleted state. F12 federation
limiter ignores trusted_proxies (whole internet shares one bucket behind
Caddy). F13 remote_media_url accepted unvalidated (javascript:/169.254.x).

## P3
F14 dead admin knobs (threads/timeout settings unwired). F15 egress pins
live forever (no TTL). F16 unbounded growth: limiter buckets/activities/
jobs/sessions/tokens. F17 nodeinfo lies (openRegistrations hardcoded,
localPosts counts remote). F18 Update(Person) ignored (stale keys 24h).
F19 config/doc drift, no toml.example, README stale. F20 create-admin
password via argv (ps-visible). F21 legacy empty csrf_token satisfies
check. F22 pipeline status guards missing; transcode re-probes; log noise.

## Checked sound (explicit)
Egress guard full-range blocklist incl v4-mapped-v6, resolve-once→pin,
TLS-only, redirects dead everywhere; GC grace closes round-2 regression;
serving range semantics RFC-correct + streamed; auth token hygiene
(hashed-at-rest, atomic single-use, timing-burn on reset); upload sniffing
+ mid-stream caps; Accept embeds Follow Mastodon-style; webfinger incl
instance actor; NodeInfo dual links; hardened unit/compose structure.

Order before deploy: F1,F2,F4 (internet-facing), then F5+F3 (first real
federation contacts), then rest.
