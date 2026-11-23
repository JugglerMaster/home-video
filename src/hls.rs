// HLS generation module.
//
// S2: copy-stream segmentation (no re-encode) for H.264/HEVC;
//     on-demand re-encode fallback for incompatible files.
//     Single ffmpeg worker, FIFO queue.
//
// See SPEC.md §7.

/// Placeholder for HLS generation state.
#[allow(dead_code)]
pub struct Hls {
    #[allow(dead_code)]
    data_dir: std::path::PathBuf,
}
