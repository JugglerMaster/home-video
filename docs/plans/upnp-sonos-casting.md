# UPnP / Sonos Casting — Plan

Cast a video's audio (audio-only first) from the home-video server to UPnP
renderers on the LAN — Sonos speakers in particular.

## Goal

From the web UI, pick a video + a discovered speaker → the video's audio plays
on the speaker at **at least** the quality of the current web player.

## Why this is non-trivial (the one real catch)

home-video streams **HLS** (`m3u8` + H.264 + AAC 192k). Sonos — and most UPnP
renderers — **do not support HLS**. They want a plain media file/stream:
MP3, AAC, ALAC, WAV, or FLAC. So we can't point a Sonos at an existing
`/stream/:id/playlist.m3u8`. We need a **direct audio endpoint** the UPnP
command can reference. That endpoint is the least obvious part of this feature.

## Quality (the user's main concern)

- Current web player audio = **AAC 192k** (set at ingest: `-b:a 192k`).
- If we serve *that same* AAC/MP3 to the speaker, quality is **identical** to
  the web player. No loss.
- Even better: stream the audio straight from the source file via `ffmpeg`
  (stream-copy if already AAC/MP3) → source audio quality, potentially better
  than the 192k HLS encode.
- Conclusion: **quality is not a blocker.** The design goal is "same as web
  player," which is achievable.

## Hard constraints

- **Target CPU:** NASOMV Intel Atom C3558 (4× 2.2 GHz ARM), low-power.
  Avoid real-time transcoding *during playback* if at all possible.
- **Source drives are read-only.** Only the `data/` dir is writable.
- **Existing:** HLS (H.264 + AAC 192k), SQLite index, Axum API, static UI,
  `ffmpeg` + `yt-dlp` in the image.
- **LAN-only:** UPnP/SSDP is multicast on the local subnet. No external
  exposure.

## Design decisions

### D1 — How to deliver audio to the renderer

**Primary: pre-bake a direct audio file at ingest.** Alongside the HLS, generate
a `.mp3` (192k) or `.m4a` (AAC 192k) file under `data/audio/<id>`. Store its
path in the DB (new column `audio_path`). Serving is a static file serve —
**zero real-time CPU**.

- Rationale: the Atom is low-power; on-demand transcoding during playback is
  too risky. Pre-baking is a one-time cost.
- Storage cost: ~192 kbps × duration. A 1-hour video ≈ 80 MB. Acceptable.

**Fallback (if storage/CPU at ingest is a problem):**
1. Stream-copy the source audio if it's already AAC/MP3 (no transcode).
2. Real-time transcode (`ffmpeg` → AAC/MP3) only as a last resort, gated behind
   a config flag and clearly documented as a CPU cost.

### D2 — SSDP + UPnP implementation

**SSDP discovery** — M-SEARCH to `239.255.255.250:1900`:
- Identify Sonos by the `ZonePlayer` URN in the response `USN`.
- Fetch each device's XML description for the friendly name + the
  `AVTransport` and `RenderingControl` control URLs.
- Refresh every ~30 s on a daemon task; keep a grace period so a single missed
  window (UDP loss) doesn't mark a healthy speaker offline.
- Two implementation options: (a) a crate (`ssdp` / `bonjour` / `multicast`),
  (b) hand-roll a small `tokio` UDP loop (~100 lines, mirrors Hearth-Connect's
  Kotlin). **Decide in Phase 0** — recommend (b) for control + no crate-risk,
  unless a crate is clearly solid.

**UPnP control** — SOAP-HTTP `POST`s to the renderer's control URL (plain HTTP,
cleartext, LAN-only — Sonos rejects the self-signed TLS cert):
- `SetVolume` (RenderingControl) — set volume **before** the URI (some firmware
  rejects it otherwise).
- `SetAVTransportURI` (AVTransport) — point the speaker at the audio URL.
- `Play` (AVTransport) — start playback.
- Implement with `reqwest`.

### D3 — UI

- **"Cast" button** in the video view + a speaker selector (discovered renderers).
- Optional later: cast directly from a Library grid cell.
- Show speaker online/offline + last-seen.

## Phased plan (tracer-bullet slices)

Each slice is independently shippable; the app is usable after each.

### Phase 0 — Spike (feasibility)
- Confirm a Sonos/UPnP renderer plays a direct MP3/AAC served by a minimal
  Rust server via UPnP.
- Pick the SSDP + UPnP approach (crate vs. hand-rolled).
- Confirm ingest-time audio pre-bake cost on the Atom (CPU + storage).
- **Acceptance:** a Sonos plays a test MP3 from a minimal Rust server.

### Phase 1 — Direct audio endpoint (vertical slice)
- Ingest: generate a direct audio file (`.mp3` 192k) alongside HLS; store
  `audio_path` in the DB.
- DB migration: add `audio_path TEXT` column (idempotent, `add_column_if_missing`).
- API: `GET /audio/:id` serves the file, auth consistent with `/stream` and
  `/thumbnail` (respect the anonymous toggle).
- **Acceptance:** `GET /audio/:id` returns a playable MP3 for an imported
  video and plays in a browser.

### Phase 2 — SSDP discovery (vertical slice)
- SSDP M-SEARCH loop (tokio UDP); identify `ZonePlayer`; fetch description
  (name + control URLs).
- Keep a list of discovered speakers; refresh every 30 s with grace period.
- Expose via a debug endpoint `GET /api/speakers` for now.
- **Acceptance:** a running Sonos appears in `GET /api/speakers`.

### Phase 3 — UPnP control (vertical slice)
- SOAP-HTTP `SetAVTransportURI` + `SetVolume` + `Play`.
- Trigger path: `POST /api/cast/:videoId/:speakerId` (and a CLI
  `home-video cast <videoId> <speakerId>` for headless testing).
- **Acceptance:** from CLI/API, a video's audio plays on a Sonos.

### Phase 4 — UI (vertical slice)
- "Cast" button in the video view + speaker selector.
- Wire to `POST /api/cast/:videoId/:speakerId`.
- **Acceptance:** from the web UI, select a video + speaker → plays on the
  Sonos.

### Phase 5 — Hardening + tests + docs
- Config: `upnp.enabled` (default `false`), `upnp.discovery_interval_s`
  (default `30`).
- Error handling: speaker offline, UPnP failure, missing audio file.
- Tests: audio endpoint (serve + format check), mock SSDP/UPnP (unit),
  integration if a renderer is available.
- Docs: README (feature + config reference).
- **Acceptance:** full flow works end-to-end on NASOMV; `cargo test` +
  `cargo clippy -- -D warnings` green; README updated.

## Open questions / risks

- **CPU at ingest:** is pre-baking acceptable on the Atom? (Phase 0 spike
  confirms.)
- **Crate vs. hand-rolled SSDP:** decide in Phase 0.
- **Video+audio on UPnP:** most renderers (Sonos) are audio-only. Video-capable
  renderers (some smart TVs) could take a direct MP4. Scope: **audio-only
  first**; video+audio is a follow-up.
- **Security:** UPnP control is LAN-only (auth via the existing API auth). The
  audio endpoint must respect the anonymous toggle.
- **Multiple speakers / groups:** out of scope (single-speaker first).

## Definition of done

- `cargo build && cargo test && cargo clippy -- -D warnings` green.
- On NASOMV: import a video → select it + a Sonos in the UI → audio plays on
  the Sonos at ≥ web-player quality.
- Source drives untouched.

## References

- Hearth-Connect's `SonosManager.kt` (discovery + UPnP + plain-HTTP clip
  delivery) — proven reference for the SSDP/UPnP/HTTP pattern.
- `PLAN.md` — overall build plan (this feature is an extension, S10+).
- `SPEC.md` — goals/non-goals (LAN-only, low-power CPU).
