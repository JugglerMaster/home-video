# home-video — Build Plan

Phased plan of **tracer-bullet** vertical slices: each slice cuts end-to-end
through schema → API → UI → tests, so the app is usable after every one.
Each slice maps to one or more issues in the issue tracker (see
`docs/agents/issue-tracker.md`).

## Completed

- **S0** (Phase 0): empty crate compiles, server boots, serves static index page.
- **S1** (Phase 1): setup wizard + config + auth. Fresh container shows the
  wizard; completing it writes `config.json` (argon2 hash), creates the schema,
  and the admin page is reachable (other routes 403 until complete). Verified
  end-to-end: `/setup` POST, `/admin/login` sets HttpOnly cookie, `/api/session`
  true→false after logout, wrong password rejected.
- **S2** (Phase 2): local auto-import. Drop a video into a watch dir → it
  appears in Library within ~30 s with thumbnail, metadata, and a working HLS
  stream. Verified end-to-end: startup scan + 30 s loop, fingerprint dedupe
  (size + mtime + first-1 MB hash), ffprobe metadata (duration/resolution/
  codec/size/bitrate), thumbnail (ffmpeg), copy-stream HLS segmentation
  (`hls/{id}/playlist.m3u8` + `.ts` segments), SQLite index; `/stream/*path`
  and `/thumbnail/*path` serve the assets; dedupe prevents re-import.
- **S3** (Phase 3): YouTube import. Paste a YouTube URL or playlist in the
  Admin tab → videos appear in Library with a matching playlist in the
  Playlist tab. Verified end-to-end: `yt-dlp` integration (single URL +
  playlist), download to `youtube_download_dir`, ingest via the Phase 2
  pipeline (dedupe, ffprobe, thumbnail, HLS), playlist auto-create
  (`source="youtube"`, `youtube_url` set, items in order), `/api/import`
  endpoint (admin-auth), `/api/playlists` and `/api/playlists/:id`
  endpoints.
- **S4** (Phase 4): video detail page (HLS player, metadata, description/tag
  editing) and FTS5-backed search in the Library. Verified end-to-end in the
  container: detail page + HLS player, `POST /api/videos/:id` edit (fixed axum
  0.7 `:id` route params — previously fell through to `/*path` → 405), FTS5
  search in Library. Regression tests cover the 405 header, POST dispatch, and
  playlist `:id` routes.
- **S5** (Phase 5): playlist create/rename/add/remove endpoints and Playlist
  tab UI (`/api/playlists`, `/api/playlists/:id`,
  `/api/playlists/:id/items`). Verified end-to-end in the container: create
  (201), rename (`PUT`, 200), add item (`POST`, 200), remove item (`DELETE`,
  200), delete playlist (`DELETE`, 200), playlist detail with item list.
- **S6** (Phase 6): video delete (DB + generated artifacts), settings GET/POST,
  and admin operations. Verified end-to-end in the container: delete video
  (DB row + HLS/thumbnail artifacts removed), settings GET (current config),
  settings POST (re-edit watch dirs / scan interval / anonymous toggle), admin
  page reachable.
- **S7** (Phase 7): GitHub Releases version check and the "update available"
  banner. Verified end-to-end in the container: `/api/version` returns current
  vs latest + update flag; UI banner (`#version-banner`) wired to the endpoint
  and shows current → latest when an update is available.
- **S8** (Phase 8): Docker/NASOMV deploy. Multi-stage `Dockerfile` (rust build
  → slim runtime w/ ffmpeg + yt-dlp), `compose.yml` (watch ro, data rw, config
  rw, port 8080), `.dockerignore`. Verified end-to-end in the container: `docker
  compose up` → setup wizard → import local video (thumbnail + HLS) → play →
  playlist → delete → version banner. NASOMV hardware check pending (port 8080).

- **S9** (Phase 9): integration test suite + docs pass. Verified: `tests/integration.rs`
  covers ingest fingerprint (deterministic; differs by content/size/mtime), HLS
  copy-stream (playlist + `.ts` segments), thumbnail (JPEG), auth (argon2
  hash/verify + malformed rejection), and the full ingest pipeline (DB row +
  HLS/thumbnail artifacts + dedupe). `cargo clippy --all-targets -- -D
  warnings` clean. README rewritten (overview, stack, quick start local +
  Docker, setup wizard, config reference, API routes, build/test/lint, deploy).

## Remaining for the overall definition of done

The NASOMV hardware check for S8 (port 8080, real hardware). Everything else
in the definition of done is met.

## Phase 0 — Repo & tooling setup

- `git init`, `.gitignore`, `README.md`.
- Cargo project: `home-video` crate, deps (axum, rusqlite, tokio, serde,
  clap, thiserror, anyhow, tokio::fs watch, reqwest, ffmpeg-cli wrappers).
- `static/` HTML/JS skeleton (four tabs, no data yet).
- CI: `cargo build`, `cargo test`, `cargo clippy -- -D warnings`.
- **Slice S0:** empty crate compiles, server boots, serves a static index page.

## Phase 1 — Config + DB + setup wizard (vertical slice)

- JSON config module (load/save/validate); default config on first run.
- SQLite schema (videos, playlists, playlist_items, FTS5).
- Setup wizard route: shown when no config; collects admin password (argon2
  hash), watch dirs, data dir, scan interval, anonymous toggle; writes config.
  Until complete, all other routes return "not configured".
- **Slice S1:** a fresh container shows the setup wizard; completing it writes
  `config.json` and the admin page is reachable (others 403).

## Phase 2 — Local auto-import (vertical slice)

- Folder watcher: startup scan + 30 s loop, recursive.
- Fingerprint dedupe (size + mtime + first-1 MB hash).
- ffprobe metadata extraction.
- Thumbnail generation (ffmpeg).
- Copy-stream HLS segmentation (no re-encode).
- Index into SQLite; Library tab renders the grid.
- **Slice S2:** drop a video into a watch dir → it appears in Library within
  ~30 s with thumbnail, metadata, and a working HLS stream.

## Phase 3 — YouTube import (vertical slice)

- yt-dlp integration (single URL + playlist).
- Cookie support.
- Download to `youtube_download_dir`, then ingest via the Phase 2 pipeline.
- Playlist → auto-create a playlist in the app containing the imported videos.
- Admin tab: paste URL/playlist → import.
- **Slice S3:** paste a YouTube playlist in Admin → videos appear in Library
  and a matching playlist exists in the Playlist tab.

## Phase 4 — Video page (vertical slice)

- Video detail route: HLS player, full metadata, tag/description editing.
- FTS5 search wired into Library (text + filters).
- **Slice S4:** open a video → it plays; edit tags/description → persists and
  search reflects it.

## Phase 5 — Playlists (vertical slice)

- User-created playlists: create, rename, add/remove videos.
- Playlist tab: list + detail (reorder, remove).
- **Slice S5:** create a playlist, add videos, view in Playlist tab; YouTube
  auto-playlists coexist.

## Phase 6 — Admin operations (vertical slice)

- Delete video (DB + generated artifacts; optional source-file delete).
- Settings: re-edit watch dirs, data dir, scan interval, anonymous toggle.
- Version status (current + last check).
- **Slice S6:** from Admin, delete a video (disappears everywhere), flip
  anonymous toggle, edit storage paths.

## Phase 7 — Auto-update notify (vertical slice)

- Version check: GitHub Releases API on admin load + every 24 h.
- Banner in UI when a newer version exists (current vs. latest + repo link).
- **Slice S7:** tag a newer release → Admin shows the "update available"
  banner with the repo link.

## Phase 8 — Docker & deploy (vertical slice)

- `Dockerfile` (multi-stage: rust build → slim runtime w/ ffmpeg + yt-dlp).
- `compose.yml`: volumes (watch dirs ro, data rw, config rw), port 8080.
- Deploy to NASOMV; verify end-to-end on the real hardware.
- **Slice S8:** `docker compose up` on NASOMV → full app works (setup →
  import → play).

## Phase 9 — Hardening

- Unit + integration tests (ingest fingerprint, HLS copy-stream, auth).
- `cargo clippy -- -D warnings` clean; docs (README: install, config, CLI).
- Edge cases: huge files, missing ffmpeg, read-only watch dir, duplicate
  fingerprints.
- **Slice S9:** test suite green, clippy clean, README complete.

## Definition of done (overall)

- `cargo build && cargo test && cargo clippy -- -D warnings` green.
- `docker compose up` on NASOMV → setup wizard → import a local video + a
  YouTube playlist → both play via HLS → playlist visible → delete works →
  version banner works.
- Source drives untouched (read-only).

## Notes

- Ordering is dependency-driven; S2 depends on S1, S3 on S2, etc.
- Each slice is independently shippable — the app is usable after S2.
- Issues are created in dependency order; each issue lists its acceptance
  criteria (the "Slice" line above is the acceptance criterion).
