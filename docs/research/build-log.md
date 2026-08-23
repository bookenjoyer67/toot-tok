# Build log — decisions & deferrals during execution

Running trail for critic rounds + owner final judgment. Newest last.

## Phase 2 → 3 transition findings

- Schema gaps reported by wave A (migrations untouched by design):
  1. `clips.has_audio` missing — probe parses it, nowhere to persist. Wave B
     stores audio presence via media_assets orig row codec info instead;
     revisit if captions/dubbing features need a real column.
  2. No `last_error` on clips — reject reasons live in `jobs.last_error`.
     Acceptable: uploader-facing error surfacing is a Phase 6 API concern.
  3. Original file path lives only in job payload — FIXED in wave B by
     inserting an `orig` media_assets row at probe success time.
- Uploads currently owned by auto-created `system` actor (clips.actor_id NOT
  NULL, no auth yet). Phase 4 replaces with real uploader attribution +
  backfills nothing (test data disposable).
- Signed expiring media URLs (ARCH §5) deliberately deferred to Phase 4:
  meaningless before visibility states exist (everything public pre-auth).
  Paired delivery: visibility column usage + signed URLs land together.

## Verification discipline observed

- Every opencode self-report re-verified by orchestrator hands:
  tests/clippy/boot/e2e curl flows. Two catches so far that self-reports
  missed (plaintext admin password P1 — caught by critic, not smoke).
- Migration immutability held: both schema gaps worked around, zero edits
  to applied migrations. 0003 added forward-only when a constraint was
  genuinely required (dedup/asset uniqueness).

## Phase 3 critic round 1 → FAIL → fix wave

- P1-1: axum DefaultBodyLimit 2MB preempted our manual cap — real uploads
  impossible. Orchestrator e2e missed it: fixture was 50KB. Lesson logged:
  e2e fixtures must exceed every threshold under test.
- P1-2: ffprobe unbounded timeout on attacker bytes = worker-slot DoS.
- P2 debt: no stale-lock reaper yet, sub-480p ladder gap (orig served
  non-faststart), whole-file RAM reads on serving, no ffmpeg thread caps,
  GC/quota unimplemented, failed-clips poison dedup (+TOCTOU on non-unique
  hash index).
- P3: dead-letter status flip, -an branch untested, worker-level tests,
  range edge cases, doc lie, settings fallback swallowing DB errors,
  HEAD/cache headers.
- All 15 dispatched as F1–F15 in one fix wave; verified-hands checklist
  after: >2MB upload e2e mandatory this time.

## Deploy kit

- Compose validated; ALPINE_LAN.md runbook written (LAN-only HTTP per owner;
  federation deferred until public domain+TLS exist). TOOTTOK_MEDIA_DIR
  wired into compose so container volume actually receives files.

## Phase 3 → CLOSED (3 rounds)

- Round 1: FAIL (2×P1: axum 2MB wall, unbounded ffprobe; +P2/P3 debt).
- Fix wave F1–F15 all landed; round 2 found the fix-wave itself had birthed
  a P1 (GC eating in-flight originals) — classic second-order bug.
- Round 3: PASS. Final state: early orig-row registration + 1h GC grace,
  kill_on_drop everywhere, job timeout 900s, guarded double-bump, 28/28
  tests, live e2e 7.5MB→ready with exact-byte ranges.
- Lesson recorded: every fix wave gets its own review round — fixes breed
  new P1s as often as they kill old ones.

## Phase 5 → CLOSED

- Wave A (follow graph, guarded resolver): 2 rounds. Self-review caught
  crate-client SSRF bypass (N1-P1) — fixed via DNS-level GuardedResolve
  plugged into FederationConfigBuilder.client + delivery client.
- Wave B (clip Create/Note federation, remote caching, deletes): one wave,
  72/72 tests incl cross-instance clip round-trip with tombstone flip and
  replay gate-skip.
- opencode beast switched deepseek-v4-flash -> openrouter/stealth/ox-alpha
  after DS credits died; owner directive: FINAL full-codebase review runs on
  ox-alpha before deploy.

## Mega-review fired

Full-codebase ox-alpha adversarial pass: cross-phase interactions,
federation correctness vs real Mastodon/Loops behavior, crash-consistency
windows, security leftovers, operational hygiene. Report-only. Fix waves
after, then Alpine deploy -> phone test.

