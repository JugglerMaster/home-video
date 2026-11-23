// Configuration module.
//
// S1: JSON config (load/save/validate), default config on first run,
// setup wizard writes it. Fields: watch_dirs, youtube_download_dir,
// data_dir, scan_interval_s, anonymous_view, admin_password_hash,
// version_check_url, port.
//
// See SPEC.md §10.

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Config {
    pub port: u16,
    pub watch_dirs: Vec<String>,
    pub youtube_download_dir: String,
    pub data_dir: String,
    pub scan_interval_s: u64,
    pub anonymous_view: bool,
    pub admin_password_hash: String,
    pub version_check_url: String,
}
