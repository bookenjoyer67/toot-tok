# TootTok container: static musl binary + ffmpeg + built frontend, Alpine base.
# Build stage uses official rust (glibc) w/ musl target; runtime = alpine.
FROM node:22-alpine AS web
WORKDIR /src
COPY web/package.json web/package-lock.json* ./
RUN npm install
COPY web ./
RUN npm run build

FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev ffmpeg-dev
WORKDIR /src
COPY . .
# Homelab laptop has 3.7G RAM — one rustc at a time keeps it alive.
ENV CARGO_BUILD_JOBS=1
RUN rustup target add x86_64-unknown-linux-musl \
 && cargo build --release --target x86_64-unknown-linux-musl -p toottok

FROM alpine:3.21
RUN apk add --no-cache ffmpeg postgresql-client
COPY --from=build /src/target/x86_64-unknown-linux-musl/release/toottok /usr/local/bin/toottok
COPY --from=build /src/crates/db/migrations /usr/local/share/toottok/migrations
COPY --from=web /src/build /usr/local/share/toottok/web
ENV TOOTTOK_MIGRATIONS_DIR=/usr/local/share/toottok/migrations
ENV TOOTTOK_WEB_DIR=/usr/local/share/toottok/web
VOLUME ["/data/media"]
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/toottok"]
