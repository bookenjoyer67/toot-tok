# TootTok

Federated short-form video for the fediverse.

TikTok-grade feel. Mastodon-grade values. Built on ActivityPub so your
videos, follows, likes and comments travel between servers — not a black
box, a network you own.

**Rust/Axum + Postgres + SvelteKit PWA. AGPLv3.**

## What it is

- **Short-form clips** — swipe feed, tap for sound, double-tap to like.
  A familiar feel, minus the algorithm feeding you.
- **Federated by default** — follows, clips, likes and comments cross
  servers as ActivityPub `Note` + video `Document` objects. Follow
  someone on another instance and their clips land in your feed.
- **Own your instance** — one binary, one Postgres, a media folder.
  Self-host on a homelab or a VPS.
- **PWA frontend** — installable, offline shell, phone-first. No app
  store, no tracking.
- **AGPL-3.0** — open source, always. Run it, fork it, read it.

## Status

Backend complete and tested — **85/85 tests, clippy zero warnings**:

| Area | What's in |
|---|---|
| Media pipeline | upload, probe, transcode ladder (faststart mp4), range serving, GC, quotas, dedup |
| Accounts & auth | Argon2id, sessions + CSRF, rate limits, trusted proxies, admin API, erasure |
| Federation | `activitypub-federation` crate, DNS-level egress guard, WebFinger, NodeInfo, Follow/Accept/Undo, clip `Create(Note)` + tombstones, idempotent inbox |
| Social API | following/discover/tag feeds, likes, announces, comments, notifications, search, reports, profiles, hashtags |
| Frontend | SvelteKit 5 SPA — swipe feed + player, upload with progress, comments sheet, notifications, search, tag pages, admin console, PWA |

Roadmap (in repo): `docs/BUILD_PLAN.md`.

## Quickstart (dev)

```sh
# own-cluster postgres (port 5433), already initialized in .pg/
pg_ctl -D .pg/data -l .pg/log -o "-p 5433 -c listen_addresses=127.0.0.1 \
  -c unix_socket_directories=$PWD/.pg" start

export PGPASSWORD=toottok
cargo run            # binds 127.0.0.1:8080, applies migrations

# frontend (separate terminal, for dev hot-reload)
cd web && npm install && npm run dev   # vite on :1420

# or serve the built SPA from the backend
cd web && npm run build && cd ..
curl localhost:8080/healthz            # expect {"status":"ok",...}
```

The backend serves the built frontend at `/` with an SPA fallback, so a
single `toottok` binary is the whole product.

## Deploy (Docker)

```sh
docker compose -f deploy/docker-compose.yml up -d --build
# first admin:
docker compose -f deploy/docker-compose.yml exec app toottok create-admin admin <password>
```

LAN install on Alpine: `deploy/ALPINE_LAN.md`.

## Config

All knobs live in `toottok.toml` or `TOOTTOK_*` env vars — bind address,
database URL, media dir, ffmpeg threads, job timeouts, trusted proxies
(behind Caddy), domain + public port for federation URLs, `behind_tls`
for cookie `Secure` flag. Example: `toottok.toml.example`.

## Docs

- Vision & scope: `docs/VISION.md`
- Architecture: `ARCHITECTURE.md`
- Build plan: `docs/BUILD_PLAN.md`
- Research (fediverse recon, CVE notes): `docs/research/`

## License

AGPL-3.0-only — see `LICENSE`. The network is the copyleft surface:
if you offer TootTok as a service, your users get the source.
