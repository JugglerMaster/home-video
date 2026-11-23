# home-video

A home video library: a web server that collects, indexes, and streams videos
from local folders and YouTube, running as a single Docker container on NASOMV.

See [SPEC.md](SPEC.md) for the full specification and
[PLAN.md](PLAN.md) for the phased build plan.

## Stack

- Rust (Axum), single crate, one binary
- SQLite (rusqlite) + FTS5
- ffmpeg + yt-dlp (sidecars in the container)
- Plain HTML/JS static UI
- Docker

## Quick start (local)

```sh
cargo run          # builds and boots the server on http://127.0.0.1:8080
```

Open http://127.0.0.1:8080 — the static UI (Library / Video / Playlist /
Admin tabs) should load. `/health` returns `ok`.

## Build / test / lint

```sh
cargo build
cargo test
cargo clippy -- -D warnings
```

## CLI (coming in S1+)

```
home-video scan [--dry-run]
home-video import <url>
home-video status
home-video set-admin-password
```

## Deployment

Docker image + `compose.yml` on NASOMV (see PLAN.md Phase 8). Port 8080.
