// YouTube import (S3).
//
// Runs yt-dlp to download a single video or playlist to a subdirectory of
// `youtube_download_dir`, then hands each file to the Phase 2 ingest pipeline.
// For playlists, a matching app playlist is auto-created (named after the
// playlist) and the imported videos are added to it.
//
// The user pastes a link (mobile or desktop); yt-dlp parses both natively,
// so the URL is passed through unchanged.

use crate::ingest::ingest;
use crate::server::AppState;
use anyhow::{Context, Result};
use std::path::PathBuf;
use tokio::task::spawn_blocking;


/// The result of a yt-dlp import.
#[derive(Debug)]
pub struct ImportResult {
    /// Number of videos ingested.
    pub ingested: usize,
    /// The playlist id, if a playlist was imported (and a playlist created).
    pub playlist_id: Option<u64>,
    /// The playlist title (if a playlist).
    pub playlist_title: Option<String>,
}

/// Video file extensions we accept as downloaded media.
const VIDEO_EXTS: [&str; 6] = ["mp4", "mkv", "webm", "mov", "flv", "avi"];

/// Run `yt-dlp --dump-json` and return the parsed metadata.
///
/// Returns a JSON object for a single video, or a JSON array (one object per
/// entry, each carrying `playlist_title`) for a playlist.
async fn yt_dump_json(url: &str) -> Result<serde_json::Value> {
    let url = url.to_string();
    let out = match spawn_blocking(move || -> anyhow::Result<std::process::Output> {
        std::process::Command::new("yt-dlp")
            .arg("--dump-json")
            .arg(&url)
            .output()
            .context("run yt-dlp --dump-json")
    })
    .await {
        Ok(inner) => inner?,
        Err(e) => return Err(anyhow::anyhow!("yt-dlp spawn: {e}")),
    };
    if !out.status.success() {
        return Err(anyhow::anyhow!(
            "yt-dlp --dump-json failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let json = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(&json)
        .with_context(|| format!("parse yt-dlp JSON: {json}"))
}

/// Download a single video or playlist to `dir`, using the video id as the
/// filename base (`{dir}/{id}.{ext}`). Returns the list of downloaded video
/// files.
async fn yt_download(url: &str, dir: &PathBuf) -> Result<Vec<PathBuf>> {
    let url = url.to_string();
    let dir_clone = dir.clone();
    let out = match spawn_blocking(move || -> anyhow::Result<std::process::Output> {
        std::process::Command::new("yt-dlp")
            // Prefer a single mp4 file (best video + best audio merged);
            // fall back to best single-file mp4, then best single-file format.
            .arg("-f")
            .arg("bestvideo[ext=mp4]+bestaudio[ext=m4a]/best[ext=mp4]/best")
            .arg("--merge-output-format")
            .arg("mp4")
            .arg("-o")
            .arg(format!("{}/%(id)s.%(ext)s", dir_clone.display()))
            .arg(&url)
            .output()
            .context("run yt-dlp download")
    })
    .await {
        Ok(inner) => inner?,
        Err(e) => return Err(anyhow::anyhow!("yt-dlp spawn: {e}")),
    };
    if !out.status.success() {
        return Err(anyhow::anyhow!(
            "yt-dlp download failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                let ext_lower = ext.to_lowercase();
                if VIDEO_EXTS.iter().any(|e| *e == ext_lower) {
                    files.push(path);
                }
            }
        }
    }
    Ok(files)
}

/// Import a YouTube URL (single video or playlist).
pub async fn import(state: &AppState, url: &str) -> Result<ImportResult> {
    // Ensure the download dir exists.
    let youtube_download_dir = {
        let cfg = state.config.lock().unwrap();
        match &*cfg {
            Some(c) => c.youtube_download_dir.clone(),
            None => return Err(anyhow::anyhow!("not configured")),
        }
    };
    let download_root = PathBuf::from(&youtube_download_dir);
    std::fs::create_dir_all(&download_root).context("create youtube_download_dir")?;

    // Metadata: determines playlist-ness and the playlist title.
    let metadata = yt_dump_json(url).await?;
    let is_playlist = metadata.is_array();
    let playlist_title = if is_playlist {
        Some(
            metadata[0]
                .get("playlist_title")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "YouTube Playlist".to_string()),
        )
    } else {
        None
    };

    // Per-video title(s) from the metadata, so imported videos are labelled
    // by their real YouTube title rather than the file stem (video id).
    // Single video: one title. Playlist: a map of video id -> title (files are
    // named {id}.{ext}, so we match on the id).
    let video_title: Option<String> = if !is_playlist {
        metadata
            .get("title")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    } else {
        None
    };
    let playlist_video_titles: Vec<(String, String)> = if is_playlist {
        if let Some(arr) = metadata.as_array() {
            arr
                .iter()
                .filter_map(|v| {
                    let id = v
                        .get("id")
                        .and_then(|t| t.as_str())
                        .map(|s| s.to_string())?
                    ;
                    let title = v
                        .get("title")
                        .and_then(|t| t.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_default();
                    Some((id, title))
                })
                .collect()
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    // A per-import subdirectory (timestamped) so downloaded files are easy to
    // find and don't collide across imports.
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let subdir = download_root.join(format!("import_{ts}"));
    std::fs::create_dir_all(&subdir).context("create import subdir")?;

    // Download.
    let files = yt_download(url, &subdir).await?;
    if files.is_empty() {
        return Err(anyhow::anyhow!("yt-dlp downloaded no video files"));
    }

    // Ingest each file via the Phase 2 pipeline.
    let mut ingested = 0;
    let mut imported_ids: Vec<u64> = Vec::new();
    for f in &files {
        let file_id = f
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .unwrap_or_default();
        let title = if is_playlist {
            playlist_video_titles
                .iter()
                .find(|(id, _)| *id == file_id)
                .map(|(_, t)| t.as_str())
        } else {
            video_title.as_deref()
        };
        match ingest(state, f, title).await {
            Ok(Some(id)) => {
                ingested += 1;
                imported_ids.push(id);
            }
            Ok(None) => {}
            Err(e) => eprintln!("import ingest error for {:?}: {e}", f),
        }
    }

    // If it was a playlist, auto-create a matching app playlist and add the
    // imported videos to it.
    let playlist_id = if is_playlist {
        let title = playlist_title.clone().unwrap_or_else(|| "YouTube Playlist".to_string());
        let db_guard = state.db.lock().unwrap();
        match &*db_guard {
            Some(db) => {
                let pid = db.create_playlist(&title, "youtube", Some(url))?;
                for (i, vid) in imported_ids.iter().enumerate() {
                    db.add_playlist_item(pid, *vid, i as i64)?;
                }
                Some(pid)
            }
            None => return Err(anyhow::anyhow!("no db in state")),
        }
    } else {
        None
    };

    Ok(ImportResult {
        ingested,
        playlist_id,
        playlist_title,
    })
}
