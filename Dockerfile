# syntax=docker/dockerfile:1

# ---------------------------------------------------------------------------
# Stage 1: build the Rust binary
# ---------------------------------------------------------------------------
FROM rust:1.97-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

# ---------------------------------------------------------------------------
# Stage 2: slim runtime with ffmpeg + yt-dlp on PATH
# ---------------------------------------------------------------------------
FROM debian:bookworm-slim

# ffmpeg provides both `ffmpeg` and `ffprobe`; yt-dlp powers YouTube import;
# ca-certificates for the version-check HTTPS call; curl for the healthcheck.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ffmpeg \
        yt-dlp \
        ca-certificates \
        curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /build/target/release/home-video ./home-video
# Static UI served by the app (Library / Video / Playlist / Admin tabs).
COPY static ./static

ENV HOME_VIDEO_PORT=8080
EXPOSE 8080

ENTRYPOINT ["./home-video"]
