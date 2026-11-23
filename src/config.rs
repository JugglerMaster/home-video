// Configuration module.
//
// S1: JSON config (load/save/validate), default config on first run,
// setup wizard writes it. Fields: watch_dirs, youtube_download_dir,
// data_dir, scan_interval_s, anonymous_view, admin_password_hash,
// version_check_url, port.
//
// See SPEC.md §10.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Default GitHub Releases check URL (notify-only, SPEC §12).
pub const DEFAULT_VERSION_CHECK_URL: &str =
    "https://api.github.com/repos/JugglerMaster/home-video/releases/latest";

#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// Build a config from setup-wizard inputs. `port` and `version_check_url`
/// default to sane values; the wizard supplies the rest.
pub fn from_setup(
    port: u16,
    watch_dirs: Vec<String>,
    youtube_download_dir: String,
    data_dir: String,
    scan_interval_s: u64,
    anonymous_view: bool,
    admin_password_hash: String,
) -> Config {
    Config {
        port,
        watch_dirs,
        youtube_download_dir,
        data_dir,
        scan_interval_s,
        anonymous_view,
        admin_password_hash,
        version_check_url: DEFAULT_VERSION_CHECK_URL.to_string(),
    }
}

/// Load config from `path`. Returns `None` if the file is missing.
pub fn load(path: &PathBuf) -> anyhow::Result<Option<Config>> {
    match std::fs::read_to_string(path) {
        Ok(json) => Ok(Some(serde_json::from_str(&json)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow::anyhow!("read config {path:?}: {e}")),
    }
}

/// Save config to `path` (creates parent dirs).
pub fn save(path: &PathBuf, config: &Config) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(config)?;
    std::fs::write(path, format!("{json}\n"))?;
    Ok(())
}

/// Validate a config (non-empty watch dirs, sane intervals/ports).
pub fn validate(config: &Config) -> anyhow::Result<()> {
    if config.watch_dirs.is_empty() {
        return Err(anyhow::anyhow!("watch_dirs must not be empty"));
    }
    if config.scan_interval_s == 0 {
        return Err(anyhow::anyhow!("scan_interval_s must be > 0"));
    }
    if config.port == 0 {
        return Err(anyhow::anyhow!("port must be > 0"));
    }
    Ok(())
}
