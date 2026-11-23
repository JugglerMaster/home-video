# home-video — Specification

A home video library: a web server that collects, indexes, and streams videos from
local folders and YouTube, running as a single Docker container on NASOMV.

## 1. Goals

- **Collect** videos from N local watch folders (auto-import) and from YouTube
  URLs / playlists (yt-dlp).
- **Index** each video: metadata (ffprobe), thumbnail, generated HLS.
- **Stream** via HLS, preferring zero-re-encode (copy-stream) so the NAS CPU
  stays near-idle during playback.
- **Organize** into playlists (auto-created for YouTube playlists, user-created).
- **Manage** via a small web UI (Library / Video / Playlist / Admin) and a CLI.
- **Notify** (not auto-execute) when a newer release is available.

## 2. Non-goals

- Multi-user accounts, sharing links, public internet exposure.
- On-demand 4K transcoding (too slow on the target CPU; copy-stream + fallback).
- Client-side editing / transcodes of source files (source drives are read-only).

## 3. Target environment

| Item | Value |
|---|---|
| Host | NASOMV, `192.168.1.33` (user `dadisc01`) |
| CPU | Intel Atom C3558 — 4× 2.2 GHz ARM cores (low-power) |
| RAM | 31 GB |
| Disk | 116 GB root (`/`), 71 GB free |
| Tooling | ffmpeg 5.1.9, Docker 29, `gh` (authed as JugglerMaster) |
| Deploy | Single Docker image + `compose.yml`, port 8080, Tailscale later |

## 4. Architecture

- **Language/stack:** Rust, single Cargo crate `home-video`, one binary.
- **Web server:** Axum (async), serves the API + static UI.
- **Modules:**
  - `server` — Axum routes, middleware (auth), static UI.
  - `ingest` — folder watcher, ffprobe, yt-dlp orchestration, thumbnails.
  - `hls` — copy-stream segmentation + on-demand re-encode fallback.
  - `db` — SQLite (rusqlite), FTS5 search.
  - `config` — JSON config load/save/validate.
- **Static UI:** plain HTML/JS in `static/` (no build step).
- **Sidecars (in same container):** `ffmpeg`, `yt-dlp`.

## 5. Collection

### 5.1 Local auto-import
- Watch N folders (configurable, one per drive). Recursive scan.
- Scan at startup + every **30 s**.
- Dedupe by fingerprint: **size + mtime + first-1 MB hash**.
- New file → ffprobe metadata (duration, resolution, codec, size, bitrate),
  thumbnail, copy-stream HLS, index into SQLite.
- Source drives are **read-only** to the app.

### 5.2 YouTube (yt-dlp)
- Single URL → download to `youtube_download_dir` (configurable; default =
  first watch folder / `youtube`).
- Playlist → download all videos **and** create a matching playlist in the app.
- Cookie support for private / age-restricted videos.

## 6. Playlists

- First-class DB entities.
- Auto-created on YouTube playlist import (named after the playlist).
- User-created: create, rename, add/remove videos.
- UI: **Playlist** tab (list + detail).

## 7. Streaming (HLS)

- Prefer **copy-stream segmentation** (no re-encode) when source is
  H.264/HEVC — lossless, near-instant.
- **Re-encode** only on-demand as a fallback for incompatible files.
- **One ffmpeg worker, FIFO queue** — playback is never starved on the Atom CPU.
- Artifacts (segments, thumbnails) stored in a configurable data dir, parallel to
  sources; sources untouched.

## 8. Auth & setup

- **First-connection setup wizard** (in-browser): shown when no JSON config
  exists. Collects admin password (hashed), watch folders, data dir, scan
  interval, anonymous toggle. Until complete, **only the setup page is
  reachable**.
- **Admin:** password-gated (delete, import, settings).
- **Anonymous view:** config toggle, **off by default**, view-only.
- After setup, the wizard is gone; re-editable via Admin settings.

## 9. Data model (SQLite)

- `videos` — id, fingerprint, path, title, description, tags, duration,
  resolution, codec, size, bitrate, created_at, updated_at, hls_m3u8,
  thumbnail_path.
- `playlists` — id, name, source (youtube/manual), youtube_url, created_at.
- `playlist_items` — playlist_id, video_id, position.
- FTS5 virtual table over `title`, `tags`, `description`.

## 10. Config (JSON)

- File on a **writable volume**; written by the setup wizard on first run,
  re-editable in Admin settings.
- Fields: `watch_dirs[]`, `youtube_download_dir`, `data_dir`, `scan_interval_s`,
  `anonymous_view` (bool, default false), `admin_password_hash`,
  `version_check_url`, `port`.

## 11. UI (tabs)

- **Library:** thumbnail grid, search/filter (resolution, codec, date, text).
- **Video:** HLS player, metadata, tag/description editing.
- **Playlist:** playlist list + detail (add/remove videos).
- **Admin:** video list w/ delete, YouTube import (URL/playlist), settings
  (re-run setup fields, storage, anonymous toggle), version status.
- **Auto-refresh:** 60 s polling of library/playlist data.

## 12. Auto-update (notify-only)

- Check GitHub Releases on admin load + every **24 h**.
- If newer → banner in UI: current vs. latest version + **link to the repo**.
- No in-app pull/restart; the user updates manually. No Docker socket mount.

## 13. CLI (same binary)

- `home-video scan [--dry-run]`
- `home-video import <url>` (single video or playlist)
- `home-video status`
- `home-video set-admin-password` (fallback to the wizard)

## 14. Deployment

- One Docker image (app + ffmpeg + yt-dlp), `compose.yml` on NASOMV.
- Port **8080**.
- Volumes: watch folders (ro), data dir (rw), config (rw).
- Tailscale added later for remote access.

## 15. Security notes

- Source drives mounted read-only.
- Admin password stored **hashed** (argon2).
- Anonymous view off by default; when on, view-only.
- No public exposure; LAN/Tailscale boundary is the trust boundary.
