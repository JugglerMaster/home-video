// Configuration module.
//
// S1: JSON config (load/save/validate), default config on first run,
// setup wizard writes it. Fields: watch_dirs, youtube_download_dir,
// data_dir, scan_interval_s, anonymous_view, admin_password_hash,
// version_check_url, port. Plus conversion settings (quality, target format,
// convert-all).
//
// See SPEC.md §10.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Default GitHub Releases check URL (notify-only, SPEC §12).
pub const DEFAULT_VERSION_CHECK_URL: &str =
    "https://api.github.com/repos/JugglerMaster/home-video/releases/latest";

/// UPnP/Sonos casting settings (docs/plans/upnp-sonos-casting.md).
///
/// `enabled` gates the whole feature (default off). `discovery_interval_s`
/// is the SSDP refresh cadence. `server_ip` is the LAN IP the renderer is
/// told to fetch audio from; empty = auto-detect.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpnpSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_upnp_discovery_interval")]
    pub discovery_interval_s: u64,
    #[serde(default)]
    pub server_ip: String,
}

impl Default for UpnpSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            discovery_interval_s: default_upnp_discovery_interval(),
            server_ip: String::new(),
        }
    }
}

fn default_upnp_discovery_interval() -> u64 {
    30
}

/// Build a config from setup-wizard inputs. `port` and `version_check_url`
/// default to sane values; the wizard supplies the rest. Conversion settings
/// take their defaults.
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
    // Conversion settings (S7+).
    #[serde(default = "default_convert_quality")]
    pub convert_quality: String,
    #[serde(default = "default_convert_format")]
    pub default_convert_format: String,
    #[serde(default = "default_convert_all")]
    pub convert_all: bool,
    // UPnP/Sonos casting (docs/plans/upnp-sonos-casting.md).
    #[serde(default)]
    pub upnp: UpnpSettings,
}

fn default_convert_quality() -> String {
    "medium".to_string()
}
fn default_convert_format() -> String {
    "h264".to_string()
}
fn default_convert_all() -> bool {
    true
}

/// Build a config from setup-wizard inputs. `port` and `version_check_url`
/// default to sane values; the wizard supplies the rest. Conversion settings
/// take their defaults.
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
        convert_quality: default_convert_quality(),
        default_convert_format: default_convert_format(),
        convert_all: default_convert_all(),
        upnp: UpnpSettings {
            enabled: false,
            discovery_interval_s: default_upnp_discovery_interval(),
            server_ip: String::new(),
        },
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
    if !["low", "medium", "high"].contains(&config.convert_quality.as_str()) {
        return Err(anyhow::anyhow!("convert_quality must be one of low, medium, high"));
    }
    if config.upnp.discovery_interval_s == 0 {
        return Err(anyhow::anyhow!("upnp.discovery_interval_s must be > 0"));
    }
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;

    fn valid_config() -> Config {
        Config {
            port: 8080,
            watch_dirs: vec!["/tmp/watch".to_string()],
            youtube_download_dir: String::new(),
            data_dir: "/tmp/data".to_string(),
            scan_interval_s: 5,
            anonymous_view: false,
            admin_password_hash: String::new(),
            version_check_url: DEFAULT_VERSION_CHECK_URL.to_string(),
            convert_quality: "medium".to_string(),
            default_convert_format: "h264".to_string(),
            convert_all: true,
            upnp: UpnpSettings {
                enabled: false,
                discovery_interval_s: 30,
                server_ip: String::new(),
            },
        }
    }

    #[test]
    fn validate_rejects_empty_watch_dirs() {
        let mut cfg = valid_config();
        cfg.watch_dirs = vec![];
        let err = validate(&cfg).unwrap_err().to_string();
        assert!(err.contains("watch_dirs"), "got: {err}");
    }

    #[test]
    fn validate_rejects_zero_scan_interval() {
        let mut cfg = valid_config();
        cfg.scan_interval_s = 0;
        let err = validate(&cfg).unwrap_err().to_string();
        assert!(err.contains("scan_interval"), "got: {err}");
    }

    #[test]
    fn validate_rejects_zero_port() {
        let mut cfg = valid_config();
        cfg.port = 0;
        let err = validate(&cfg).unwrap_err().to_string();
        assert!(err.contains("port"), "got: {err}");
    }

    #[test]
    fn validate_rejects_bad_convert_quality() {
        for bad in ["medium!", "", "ultra"] {
            let mut cfg = valid_config();
            cfg.convert_quality = bad.to_string();
            let err = validate(&cfg).unwrap_err().to_string();
            assert!(err.contains("convert_quality"), "got: {err}");
        }
    }

    #[test]
    fn validate_rejects_zero_upnp_interval() {
        let mut cfg = valid_config();
        cfg.upnp.discovery_interval_s = 0;
        let err = validate(&cfg).unwrap_err().to_string();
        assert!(err.contains("upnp"), "got: {err}");
    }

    #[test]
    fn validate_accepts_valid_config() {
        validate(&valid_config()).unwrap();
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let cfg = valid_config();
        save(&path, &cfg).unwrap();
        let loaded = load(&path).unwrap().expect("config file must exist");
        assert_eq!(loaded.port, cfg.port);
        assert_eq!(loaded.watch_dirs, cfg.watch_dirs);
        assert_eq!(loaded.scan_interval_s, cfg.scan_interval_s);
        assert_eq!(loaded.convert_quality, cfg.convert_quality);
        assert_eq!(loaded.upnp.discovery_interval_s, cfg.upnp.discovery_interval_s);
    }

    #[test]
    fn load_returns_none_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.json");
        let loaded = load(&path).unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn from_setup_defaults_upnp_off_and_version_url() {
        let cfg = from_setup(
            8080,
            vec!["/tmp/watch".to_string()],
            "".to_string(),
            "/tmp/data".to_string(),
            5,
            false,
            String::new(),
        );
        assert!(!cfg.upnp.enabled);
        assert_eq!(cfg.upnp.discovery_interval_s, 30);
        assert!(cfg.version_check_url.contains("github.com"));
        assert_eq!(cfg.convert_quality, "medium");
        assert_eq!(cfg.default_convert_format, "h264");
        assert!(cfg.convert_all);
    }
}
