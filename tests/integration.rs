// Integration tests for S9: ingest fingerprint, HLS copy-stream, auth, and
// the full ingest pipeline.
//
// Uses real ffmpeg/ffprobe (present in the runtime image and dev env) and a
// real temp DB, so these catch regressions in the external-binary paths.

use home_video::{config::Config, db::Db, ingest, server::AppState};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

// Keep temp dirs alive for the whole test run (a TempDir drops its contents
// when it goes out of scope, which would delete the DB/files mid-test).
static TEMP_DIRS: std::sync::Mutex<Vec<tempfile::TempDir>> =
    std::sync::Mutex::new(Vec::new());

fn keep_dir(dir: tempfile::TempDir) {
    TEMP_DIRS.lock().unwrap().push(dir);
}

/// Create a temp dir and return its path. The dir itself is kept alive in the
/// static so the path stays valid for the test's duration.
fn temp_dir() -> PathBuf {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    keep_dir(dir);
    path
}

fn new_db() -> (Db, PathBuf) {
    let dir = temp_dir();
    let path = dir.join("test.db");
    let db = Db::new(&path).unwrap();
    db.create_schema().unwrap();
    (db, path)
}

/// Generate a short test video (2s, 320x240, 10fps) with ffmpeg into `dir`.
fn make_video(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    let output = std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=2:size=320x240:rate=10",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            path.to_str().unwrap(),
        ])
        .output()
        .expect("spawn ffmpeg");
    assert!(
        output.status.success(),
        "ffmpeg failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    path
}

fn make_state(data_dir: PathBuf, db: Db) -> AppState {
    AppState {
        config_path: data_dir.join("config.json"),
        port: 8080,
        config: {
            let cfg = Config {
                port: 8080,
                watch_dirs: vec![],
                youtube_download_dir: String::new(),
                data_dir: data_dir.to_string_lossy().to_string(),
                scan_interval_s: 5,
                anonymous_view: true,
                admin_password_hash: String::new(),
                version_check_url: String::new(),
                convert_quality: "medium".to_string(),
                default_convert_format: "h264".to_string(),
                convert_all: true,
            };
            Arc::new(Mutex::new(Some(cfg)))
        },
        sessions: Arc::new(Mutex::new(HashSet::new())),
        db: Arc::new(Mutex::new(Some(db))),
        conversions: Arc::new(Mutex::new(Vec::new())),
    }
}

/// Default conversion settings (medium / h264 / convert-all on).
fn default_convert() -> ingest::ConvertSettings {
    ingest::ConvertSettings {
        quality: "medium".to_string(),
        target_format: "h264".to_string(),
        convert_all: true,
    }
}

// ---------------------------------------------------------------------------
// Fingerprint
// ---------------------------------------------------------------------------

#[test]
fn fingerprint_is_deterministic() {
    let dir = temp_dir();
    let path = make_video(&dir, "a.mp4");
    let a = ingest::fingerprint(&path).unwrap();
    let b = ingest::fingerprint(&path).unwrap();
    assert_eq!(a, b);
}

#[test]
fn fingerprint_differs_by_content() {
    let dir = temp_dir();
    let path_a = dir.join("a.mp4");
    let path_b = dir.join("b.mp4");
    std::fs::write(&path_a, b"AAAA".repeat(100)).unwrap();
    std::fs::write(&path_b, b"BBBB".repeat(100)).unwrap();
    assert_ne!(
        ingest::fingerprint(&path_a).unwrap(),
        ingest::fingerprint(&path_b).unwrap()
    );
}

#[test]
fn fingerprint_differs_by_size() {
    let dir = temp_dir();
    let path_a = dir.join("a.mp4");
    let path_b = dir.join("b.mp4");
    std::fs::write(&path_a, b"x".repeat(1000)).unwrap();
    std::fs::write(&path_b, b"x".repeat(2000)).unwrap();
    assert_ne!(
        ingest::fingerprint(&path_a).unwrap(),
        ingest::fingerprint(&path_b).unwrap()
    );
}

#[test]
fn fingerprint_differs_by_mtime() {
    let dir = temp_dir();
    let path_a = dir.join("a.mp4");
    let path_b = dir.join("b.mp4");
    std::fs::write(&path_a, b"x".repeat(1000)).unwrap();
    // Wait so path_b's mtime lands in a different second than path_a's.
    std::thread::sleep(std::time::Duration::from_secs(2));
    std::fs::write(&path_b, b"x".repeat(1000)).unwrap();
    assert_ne!(
        ingest::fingerprint(&path_a).unwrap(),
        ingest::fingerprint(&path_b).unwrap()
    );
}

// ---------------------------------------------------------------------------
// HLS copy-stream
// ---------------------------------------------------------------------------

#[tokio::test]
async fn hls_generates_playlist_and_segments() {
    let dir = temp_dir();
    let src = make_video(&dir, "src.mp4");
    let out_dir = dir.join("hls_out");
    ingest::hls(&src, &out_dir, "h264", &default_convert()).expect("hls");
    let playlist = out_dir.join("playlist.m3u8");
    assert!(playlist.exists(), "playlist.m3u8 missing");
    let m3u8 = std::fs::read_to_string(&playlist).unwrap();
    assert!(m3u8.starts_with("#EXTM3U"), "not an m3u8 playlist: {m3u8:?}");
    // At least one .ts segment referenced.
    let segments: Vec<_> = m3u8.lines().filter(|l| l.ends_with(".ts")).collect();
    assert!(!segments.is_empty(), "no .ts segments in playlist");
    for seg in &segments {
        assert!(out_dir.join(seg).exists(), "segment {seg} missing");
    }
}

// ---------------------------------------------------------------------------
// HLS re-encode (non copy-stream codec)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn hls_reencodes_non_copy_codec() {
    let dir = temp_dir();
    let src = make_video(&dir, "src.mp4");
    let out_dir = dir.join("hls_reenc_out");
    // Force the re-encode branch by passing a non copy-stream codec.
    ingest::hls(&src, &out_dir, "av1", &default_convert()).expect("hls reencode");
    let playlist = out_dir.join("playlist.m3u8");
    assert!(playlist.exists(), "playlist.m3u8 missing");
    let m3u8 = std::fs::read_to_string(&playlist).unwrap();
    assert!(m3u8.starts_with("#EXTM3U"), "not an m3u8 playlist: {m3u8:?}");
    let segments: Vec<_> = m3u8.lines().filter(|l| l.ends_with(".ts")).collect();
    assert!(!segments.is_empty(), "no .ts segments in playlist");
    for seg in &segments {
        assert!(out_dir.join(seg).exists(), "segment {seg} missing");
    }
}

// ---------------------------------------------------------------------------
// HLS: conversion disabled leaves non-playback codecs unconverted
// ---------------------------------------------------------------------------

#[tokio::test]
async fn hls_refuses_non_playback_when_conversion_off() {
    let dir = temp_dir();
    let src = make_video(&dir, "src.mp4");
    let out_dir = dir.join("hls_conv_off");
    let off = ingest::ConvertSettings {
        quality: "medium".to_string(),
        target_format: "h264".to_string(),
        convert_all: false,
    };
    // av1 is not playback-supported and conversion is off → must fail.
    let result = ingest::hls(&src, &out_dir, "av1", &off);
    assert!(result.is_err(), "expected hls to fail when conversion is off");
    assert!(
        !out_dir.join("playlist.m3u8").exists(),
        "no playlist should be written when conversion is off"
    );
}

// ---------------------------------------------------------------------------
// HLS: copy-stream codecs are unaffected by conversion settings
// ---------------------------------------------------------------------------

#[tokio::test]
async fn hls_copy_stream_ignores_convert_all_off() {
    let dir = temp_dir();
    let src = make_video(&dir, "src.mp4");
    let out_dir = dir.join("hls_copy_conv_off");
    let off = ingest::ConvertSettings {
        quality: "medium".to_string(),
        target_format: "h264".to_string(),
        convert_all: false,
    };
    // h264 is playback-supported → copy-stream even when conversion is off.
    ingest::hls(&src, &out_dir, "h264", &off).expect("hls copy-stream with convert_all off");
    let playlist = out_dir.join("playlist.m3u8");
    assert!(playlist.exists(), "playlist.m3u8 missing");
}

// ---------------------------------------------------------------------------
// Thumbnail
// ---------------------------------------------------------------------------

#[tokio::test]
async fn thumbnail_creates_jpeg() {
    let dir = temp_dir();
    let src = make_video(&dir, "src.mp4");
    let out = dir.join("thumb.jpg");
    ingest::thumbnail(&src, &out).expect("thumbnail");
    assert!(out.exists(), "thumbnail jpg missing");
    assert!(out.metadata().unwrap().len() > 0, "thumbnail jpg empty");
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

#[test]
fn auth_hash_and_verify_roundtrip() {
    let hash = home_video::auth::hash_password("correct-horse");
    assert!(home_video::auth::verify_password("correct-horse", &hash));
    assert!(!home_video::auth::verify_password("wrong", &hash));
}

#[test]
fn auth_verify_rejects_malformed_hash() {
    assert!(!home_video::auth::verify_password("anything", "not-a-valid-hash"));
}

// ---------------------------------------------------------------------------
// Full ingest pipeline
// ---------------------------------------------------------------------------

#[tokio::test]
async fn full_ingest_pipeline_creates_video_and_artifacts() {
    let dir = temp_dir();
    let data_dir = dir.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let (db, _db_path) = new_db();
    let state = make_state(data_dir.clone(), db);
    let src = make_video(&dir, "src.mp4");

    let id = ingest::ingest(&state, &src, None)
        .await
        .expect("ingest")
        .expect("should ingest (new fingerprint)");
    assert!(id > 0);

    // DB row with metadata.
    let videos = state
        .db
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .get_videos()
        .unwrap();
    assert!(videos.iter().any(|v| v.id == id), "video not in DB");
    let v = &videos[0];
    assert!(v.duration > 0.0, "duration zero");
    assert!(!v.resolution.is_empty(), "resolution empty");
    assert!(!v.codec.is_empty(), "codec empty");
    let m3u8 = v.hls_m3u8.clone();
    assert!(m3u8.ends_with("playlist.m3u8"), "hls_m3u8 path wrong: {m3u8}");
    let thumb = v.thumbnail_path.clone();
    assert!(thumb.ends_with(".jpg"), "thumbnail_path wrong: {thumb}");

    // HLS artifacts on disk.
    let hls_dir = data_dir.join("hls").join(id.to_string());
    assert!(hls_dir.join("playlist.m3u8").exists(), "hls playlist missing");
    assert!(
        data_dir.join("thumbnails").join(format!("{id}.jpg")).exists(),
        "thumbnail missing"
    );
}

#[tokio::test]
async fn full_ingest_dedupe_skips_existing_fingerprint() {
    let dir = temp_dir();
    let data_dir = dir.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let (db, _db_path) = new_db();
    let state = make_state(data_dir.clone(), db);
    let src = make_video(&dir, "src.mp4");

    let id1 = ingest::ingest(&state, &src, None)
        .await
        .expect("first ingest")
        .expect("should ingest");
    assert!(id1 > 0);

    // Same file again → same fingerprint → dedupe (None).
    let id2 = ingest::ingest(&state, &src, None).await.expect("second ingest");
    assert!(id2.is_none(), "second ingest should be deduped (None)");

    // Only one video in DB.
    let count = state
        .db
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .get_videos()
        .unwrap()
        .len();
    assert_eq!(count, 1, "expected 1 video, got {count}");
}
