# TootTok

> **Status:** NAME LOCKED by owner (was treated as placeholder in error).
>
> A federated, self-hosted short-form video platform for the fediverse.
> TikTok-grade feel. Mastodon-grade values. One binary + Postgres + ffmpeg.

## One-liner

Short-form vertical video, federated over ActivityPub. Anyone can run a
server; every server speaks to the whole fediverse; non-technical people can
install and use it without reading a wiki.

## Why now

The fediverse covers text (Mastodon, Akkoma, GoToSocial), photos (Pixelfed),
and long video (PeerTube). Short-form video is its smallest corner: per
fediverse.observer stats (as of March 2026), Loops — the main project in
this space — runs ~31 servers, ~40.8K accounts, ~4.1K monthly actives. The
niche is real but nearly empty. Room exists for a second, independent
implementation in a different language with a different ops story — exactly
how Mastodon, Akkoma, Pleroma and GoToSocial coexist and interoperate.

We are building a colleague for Loops, not a replacement. Both projects
federate with each other and with everything else.

## Who it is for

1. **Viewers** who want a scroll feed that feels like the apps they know.
2. **Creators** who want uploads that just work, with no algorithm lottery.
3. **Admins** — including non-technical ones — who want an install measured
   in minutes, not weekends.
4. **Developers** who get an open, documented REST API usable from any
   language, because the protocol and code are open (AGPLv3).

## Product pillars (non-negotiable)

1. **Federation is invisible.** Users never see "instance", "ActivityPub"
   or "WebFinger" unless they go looking. Following `@someone@remote.example`
   works from the normal search box.
2. **Thumb-first UX parity.** Vertical swipe feed, tap-to-pause, sound-off
   by default, one-hand reachable controls. Muscle memory from mainstream
   apps transfers 1:1.
3. **Zero-thought onboarding.** Sign up → follow suggested accounts from a
   curated starter pack (bundled, admin-editable, skippable) → watching.
   Fresh/empty servers fall back to Discover instead of a dead feed.
4. **Ethical feeds.** "Following" is strictly chronological. Any ranked feed
   is opt-in, explains its signals, no engagement dark patterns. No ads,
   ever. No selling data, ever.
5. **Moderation is built in, not bolted on.** Reports, content warnings,
   domain blocks, importable blocklists at v1. Account self-deletion with
   real erasure (personal data purged, tombstones kept) ships at v1 —
   open registration without an exit is unacceptable.
6. **Admin delight.** Minimum install: our binary + Postgres + ffmpeg +
   a media dir — behind Caddy (compose default) TLS is automatic because
   federation REQUIRES valid HTTPS. Plain-English errors. Sensible defaults.

## Scope discipline (what v1 does NOT have)

Learned from watching feature sprawl slow other projects down. v1 ships:

- NO direct messages
- NO duets / stitches / remixes
- NO live streaming
- NO playlists
- NO mobile native apps (PWA covers phones — client strategy is decision D2)
- NO WebTorrent/P2P delivery
- NO algorithmic "For You" as default (opt-in Discover only)
- NO custom themes/plugins

Everything above is designed-for-later (schema/API leave room), none of it
blocks v1.

## Values borrowed from colleagues

| From | What we take |
|---|---|
| Loops | Note+video-attachment interop shape (verified vs their validator); sounds-table concept; trust-score moderation direction |
| PeerTube | Transcode pipeline order (probe → ladder → publish → THEN federate); faststart discipline; remote-runner concept later |
| Mastodon | HTTP signature discipline, domain blocks, NodeInfo, shared-inbox etiquette |
| GoToSocial | Small-server resource humility; single-binary ops |

## Success criteria for v1

- A stranger installs via Docker Compose (Caddy included) in under 15
  minutes on a $6 VPS **with a real domain and working certificate**, then
  registers and uploads successfully.
- A Mastodon user follows a creator on us, sees new clips in their home
  timeline, plays them INLINE (our mp4 routes serve correct Content-Type +
  range support), likes and replies — all round-trips verified by script.
- Lighthouse ≥ 90 performance and accessibility on the PWA.
- Two of our servers + one Mastodon + one Loops instance pass the scripted
  federation smoke test (follow, post, like, comment, delete propagate).
- Account deletion round-trips: erase locally, tombstone remotely, Mastodon
  sees the account go.
- Zero known unfixed high CVEs in dependencies at release (incl. re-check
  of activitypub_federation advisory state at freeze).

## Naming

Name **TootTok — LOCKED by owner.** Note for later: "Tok" sits close to
TikTok's mark; before any public launch, run a quick trademark sanity pass
(free advice from an IP-minded friend is enough at this scale). Not a
blocker for development under the name.
