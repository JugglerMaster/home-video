// Ingestion pipeline (S2).
//
// Scans watch dirs, dedupes by fingerprint (size + mtime + first-1MB sha256),
// extracts metadata with ffprobe, generates a thumbnail with ffmpeg,
// segments to HLS (copy-stream when possible, re-encode otherwise), and
// indexes into SQLite.
//
// Layout under data_dir:
//   home-video.db
//   hls/{video_id}/playlist.m3u8 + seg_*.ts
//   thumbnails/{video_id}.jpg

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::db::Video;
use crate::server::AppState;

/// Video file extensions we recognize (case-insensitive).
const VIDEO_EXTS: [&str; 11] = [
    "mp4", "mkv", "avi", "mov", "webm",
    "m4v", "flv", "wmv", "ts", "m2ts", "3gp",
];

/// Video codecs that can be copy-streamed into HLS without re-encoding
/// (SPEC \u00a77). Everything else is re-encoded to a universally playable
/// codec (H.264 + AAC) so it plays in all browsers.
const COPY_STREAM_CODECS: [&str; 2] = ["h264", "hevc"];

/// ffprobe metadata.
#[derive(Clone, Debug)]
pub struct Probe {
    pub duration: f64,
    pub size: i64,
    pub bitrate: i64,
    pub resolution: String,
    pub codec: String,
}

fn now() -> String {
    let t = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    format!("{}.{}", t.as_secs(), t.subsec_millis())
}

/// Recursive video file scan under `dirs`.
pub fn scan_video_files(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for dir in dirs {
        scan_dir(dir, &mut out);
    }
    out
}

fn scan_dir(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let path = entry.path();
            if path.is_dir() {
                scan_dir(&path, out);
            } else if path.is_file() {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    if VIDEO_EXTS.iter().any(|v| v.eq_ignore_ascii_case(ext)) {
                        out.push(path);
                    }
                }
            }
        }
    }
}

/// Fingerprint = sha256("{size} {mtime} {first_1mb_hash}").
pub fn fingerprint(path: &Path) -> Result<String> {
    let meta = fs::metadata(path)?;
    let size = meta.len() as i64;
    let mtime = meta.modified()?;
    // First 1 MB hash.
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    let mut reader = fs::File::open(path)?;
    let mut total = 0usize;
    while total < 1024 * 1024 {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n;
    }
    let first_1mb = hex::encode(hasher.finalize());
    let mtime_s = mtime.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let fp = sha256_string(&format!("{size} {mtime_s} {first_1mb}"));
    Ok(fp)
}

fn sha256_string(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// Extract metadata with ffprobe.
pub fn probe(path: &Path) -> Result<Probe> {
    let output = std::process::Command::new("ffprobe")
        .args([
            "-v", "error",
            "-show_entries", "format=duration,size,bit_rate",
            "-show_entries", "stream=codec_name,width,height",
            "-of", "json",
            path.to_str().unwrap_or_default(),
        ])
        .output()
        .context("ffprobe")?;
    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "ffprobe failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let val: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout))?;
    // ffprobe emits numeric fields as strings; parse accordingly.
    let format = &val["format"];
    let empty: Vec<serde_json::Value> = Vec::new();
    let streams = val["streams"].as_array().unwrap_or(&empty);
    let duration: f64 = format["duration"]
        .as_f64()
        .or_else(|| format["duration"].as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0.0);
    let size: i64 = format["size"]
        .as_i64()
        .or_else(|| format["size"].as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0);
    let bitrate: i64 = format["bit_rate"]
        .as_i64()
        .or_else(|| format["bit_rate"].as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0);
    let (resolution, codec) = streams
        .iter()
        .find(|s| s.get("width").is_some() && s.get("height").is_some())
        .map(|s| {
            let w = s["width"].as_i64().unwrap_or(0);
            let h = s["height"].as_i64().unwrap_or(0);
            let c = s["codec_name"].as_str().unwrap_or("unknown");
            (format!("{w}x{h}"), c.to_string())
        })
        .unwrap_or_else(|| ("0x0".to_string(), "unknown".to_string()));
    Ok(Probe {
        duration,
        size,
        bitrate,
        resolution,
        codec,
    })
}

/// Generate a thumbnail (ffmpeg, 10s in, 480p wide).
///
/// Tries a frame at 10s first; falls back to 0s if the video is shorter than
/// 10s. ffmpeg exits 0 even when it produces no frame (e.g. seeking past the
/// end), so each attempt is verified by checking the output file actually
/// exists and is non-empty rather than trusting the exit code.
fn thumbnail_attempt(src: &Path, ss: &str, out: &Path) -> Result<bool> {
    if out.exists() && fs::remove_file(out).is_err() {
        return Ok(false);
    }
    let output = std::process::Command::new("ffmpeg")
        .args([
            "-y", "-ss", ss,
            "-i", src.to_str().unwrap_or_default(),
            "-frames:v", "1",
            "-vf", "scale=480:-2",
            "-q:v", "5",
            out.to_str().unwrap_or_default(),
        ])
        .output()
        .context("ffmpeg thumbnail")?
        ;
    if !output.status.success() {
        return Ok(false);
    }
    let ok = out.exists()
        && out
            .metadata()
            .map(|m| m.len() > 0)
            .unwrap_or(false);
    if !ok {
        eprintln!(
            "thumbnail attempt (ss={ss}) produced no file: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(ok)
}

pub fn thumbnail(src: &Path, out: &Path) -> Result<()> {
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent)?;
    }
    // Try 10s in first; fall back to 0s for short videos. A spawn failure
    // (e.g. missing ffmpeg) propagates as an error rather than panicking.
    if thumbnail_attempt(src, "00:00:10", out)? {
        return Ok(());
    }
    if thumbnail_attempt(src, "0", out)? {
        return Ok(());
    }
    Err(anyhow::anyhow!("thumbnail failed for {}", src.display()))
}

/// Segment to HLS. Copy-stream (no re-encode) when the source video codec is
/// H.264/HEVC; otherwise re-encode to a universally playable codec (H.264 +
/// AAC) so the result plays in all browsers. Writes playlist.m3u8 + seg_*.ts
/// into out_dir.
pub fn hls(src: &Path, out_dir: &Path, codec: &str) -> Result<()> {
    fs::create_dir_all(out_dir)?;
    let src_str = src.display().to_string();
    let mut args: Vec<String> = vec!["-y".into(), "-i".into(), src_str];
    if COPY_STREAM_CODECS.contains(&codec) {
        args.extend(["-c".into(), "copy".into()]);
    } else {
        // Re-encode to H.264 + AAC (universally playable in browsers).
        args.extend(
            [
                "-c:v", "libx264", "-crf", "23", "-preset", "medium",
                "-c:a", "aac", "-b:a", "192k",
            ]
            .iter()
            .map(|s| s.to_string()),
        );
    }
    args.extend(
        [
            "-f", "hls",
            "-hls_time", "6",
            "-hls_list_size", "0",
            "-hls_segment_filename", "seg_%03d.ts",
            "playlist.m3u8",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    let output = std::process::Command::new("ffmpeg")
        .current_dir(out_dir)
        .args(&args)
        .output()
        .context("ffmpeg hls")?;
    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "hls failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

/// Ingest a single video file into the app.
pub async fn ingest(state: &AppState, path: &Path) -> Result<Option<u64>> {
    let fp = fingerprint(path)?;

    // Dedupe.
    {
        let db_guard = state.db.lock().unwrap();
        if let Some(db) = db_guard.as_ref() {
            if db.find_by_fingerprint(&fp)?.is_some() {
                return Ok(None);
            }
        }
    }

    let probe = probe(path)?;

    let data_dir = {
        let cfg = state.config.lock().unwrap();
        match &*cfg {
            Some(c) => c.data_dir.clone(),
            None => return Err(anyhow::anyhow!("not configured")),
        }
    };
    let hls_dir = PathBuf::from(&data_dir).join("hls");
    let thumb_dir = PathBuf::from(&data_dir).join("thumbnails");
    fs::create_dir_all(&hls_dir)?;
    fs::create_dir_all(&thumb_dir)?;

    // Insert (placeholder paths) to get an ID.
    let now = now();
    let video_id = {
        let db_guard = state.db.lock().unwrap();
        match &*db_guard {
            Some(db) => {
                let v = Video {
                    id: 0,
                    fingerprint: fp.clone(),
                    path: path.display().to_string(),
                    title: path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .map(|s| s.to_string())
                        .unwrap_or_default(),
                    description: String::new(),
                    tags: String::new(),
                    duration: probe.duration,
                    resolution: probe.resolution.clone(),
                    codec: probe.codec.clone(),
                    size: probe.size,
                    bitrate: probe.bitrate,
                    created_at: now.clone(),
                    updated_at: now.clone(),
                    hls_m3u8: String::new(),
                    thumbnail_path: String::new(),
                };
                db.insert_video(&v)?
            }
            None => return Err(anyhow::anyhow!("no db in state")),
        }
    };

    // Process (best-effort: a single bad file must not abort the scan; a
    // video that fails to become playable is still indexed and flagged).
    let hls_dir_id = hls_dir.join(video_id.to_string());
    fs::create_dir_all(&hls_dir_id)?;
    let thumb_path = thumb_dir.join(format!("{}.jpg", video_id));
    let thumb_ok = thumbnail(path, &thumb_path).is_ok();
    if !thumb_ok {
        eprintln!(
            "video {video_id}: thumbnail failed (continuing; video may lack a thumbnail)"
        );
    }
    let codec = probe.codec.clone();
    let codec_owned = codec.clone();
    let src_owned = path.to_path_buf();
    let hls_dir_id_owned = hls_dir_id.clone();
    let hls_ok = match tokio::task::spawn_blocking(move || {
        hls(&src_owned, &hls_dir_id_owned, &codec_owned)
    })
    .await
    {
        Ok(inner) => inner.is_ok(),
        Err(e) => {
            eprintln!("video {video_id}: hls join error: {e}");
            false
        }
    };
    if !hls_ok {
        eprintln!(
            "video {video_id}: could not produce playable HLS (codec={codec}); marking unplayable"
        );
    }

    // Update DB.
    {
        let db_guard = state.db.lock().unwrap();
        if let Some(db) = &*db_guard {
            let m3u8 = if hls_ok { format!("{video_id}/playlist.m3u8") } else { String::new() };
            let thumb = if thumb_ok { format!("{video_id}.jpg") } else { String::new() };
            db.update_paths(video_id, &m3u8, &thumb)?;
        }
    }

    println!("ingested {} (id={})", path.display(), video_id);
    Ok(Some(video_id))
}

/// Run a full scan of all watch dirs.
pub async fn scan(state: &AppState) -> Result<usize> {
    // Lock only to read the watch dirs; the guard must be dead before the
    // await so the future stays Send.
    let dirs: Vec<PathBuf> = {
        let cfg = state.config.lock().unwrap();
        match &*cfg {
            Some(c) => c.watch_dirs.iter().map(PathBuf::from).collect(),
            None => return Err(anyhow::anyhow!("not configured")),
        }
    };
    let files = scan_video_files(&dirs);
    let mut ingested = 0;
    for f in files {
        match ingest(state, &f).await {
            Ok(Some(_)) => ingested += 1,
            Ok(None) => {}
            Err(e) => eprintln!("scan ingest error for {:?}: {e}", f),
        }
    }
    Ok(ingested)
}
