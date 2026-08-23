# TootTok on the Alpine homelab (LAN-only, phone testing)

Owner decision: LAN HTTP only for now — no domain, no Cloudflare, no TLS.
Federation stays OFF-by-default shape until public deployment (needs real
domain + HTTPS); LAN mode is for interface testing on phones.

## One-time prep (on the Alpine laptop, 192.168.1.100)

1. Install docker (or docker-engine) + docker-compose plugin:
   `apk add docker docker-cli-compose` && `rc-update add docker boot` &&
   `service docker start`
2. Copy this repo's `deploy/` folder to the box (scp or git once pushed):
   `scp -r deploy root@192.168.1.100:/opt/toottok-deploy`

## Build + run

```sh
cd /opt/toottok-deploy
docker compose build          # builds musl static binary + ffmpeg image
docker compose up -d
docker compose logs -f app    # expect: migrations applied, listening :8080
```

3. First admin:
```sh
docker compose exec app toottok create-admin admin <pick-password>
```

## Phone test

- Phone on same WiFi → browser → `http://192.168.1.100:8080/healthz`
- Expect `{"status":"ok","service":"toottok"}`

## Firewall note (Alpine side)

If awall/iptables active, allow 8080/tcp from LAN range only.

## Data lives where?

- Postgres data: named volume `pgdata` (survives rebuilds)
- Media files: named volume `media`

## Teardown / reset

```sh
docker compose down            # keeps volumes
docker compose down -v         # NUKES db + media
```
