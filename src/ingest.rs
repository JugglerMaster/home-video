// Ingestion module (folder watcher + ffprobe + yt-dlp).
//
// S2: folder watcher (startup scan + 30s loop, recursive), fingerprint
//     dedupe (size + mtime + first-1MB hash), ffprobe metadata, thumbnails.
// S3: yt-dlp (single URL + playlist), cookie support.
//
// See SPEC.md §5.

/// Placeholder for ingestion state.
#[allow(dead_code)]
pub struct Ingest {
    #[allow(dead_code)]
    watch_dirs: Vec<std::path::PathBuf>,
}
