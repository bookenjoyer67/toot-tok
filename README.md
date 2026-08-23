# TootTok

Federated short-form video for the fediverse. TikTok-grade feel,
Mastodon-grade values. Rust/Axum + Postgres + SvelteKit PWA. AGPLv3.

- Vision: `docs/VISION.md`
- Architecture: `ARCHITECTURE.md`
- Build plan: `docs/BUILD_PLAN.md`
- Research: `docs/research/`

Status: Phase 1 — workspace skeleton.

## Dev quickstart

```sh
# own-cluster postgres (port 5433), already initialized in .pg/
pg_ctl -D .pg/data -l .pg/log -o "-p 5433 -c listen_addresses=127.0.0.1 \
  -c unix_socket_directories=$PWD/.pg" start

export PGPASSWORD=toottok
cargo run            # binds 127.0.0.1:8080, applies migrations
curl localhost:8080/healthz
```

License: AGPL-3.0-only (see LICENSE).
