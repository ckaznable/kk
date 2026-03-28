use std::{env, path::PathBuf, sync::LazyLock};

use directories::ProjectDirs;
use serde::Deserialize;

pub static DIR: LazyLock<ProjectDirs> = LazyLock::new(|| ProjectDirs::from("", "", "kk").unwrap());

pub static THUMB_CACHE_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
    let dir = DIR.cache_dir().join("thumbs");
    if !dir.exists() {
        std::fs::create_dir_all(&dir).ok();
    }
    dir
});

pub static SEARCH_PATH: LazyLock<PathBuf> = LazyLock::new(|| {
    let search_path = env::var("KK_SEARCH_PATH").expect("KK_SEARCH_PATH env variable is required");
    PathBuf::from(search_path)
});

pub static WEBDAV_URL: LazyLock<Option<String>> = LazyLock::new(|| env::var("KK_WEBDAV_URL").ok());
pub static WEBDAV_USER: LazyLock<Option<String>> =
    LazyLock::new(|| env::var("KK_WEBDAV_USER").ok());
pub static WEBDAV_PASS: LazyLock<Option<String>> =
    LazyLock::new(|| env::var("KK_WEBDAV_PASS").ok());

pub static JAVDB_COOKIE: LazyLock<Option<String>> =
    LazyLock::new(|| env::var("KK_JAVDB_COOKIE").ok());

// ── config.toml ──────────────────────────────────────────────────────────────

#[derive(Deserialize, Debug, Clone, Default)]
pub struct AppConfig {
    pub ks: Option<KsConfig>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct KsConfig {
    pub base_url: Option<String>,
    pub download_max_total_bytes: Option<u64>,
    pub download_cache_dir: Option<String>,
}

/// Path to config.toml: `<config_local_dir>/config.toml`
pub fn config_toml_path() -> PathBuf {
    DIR.config_local_dir().join("config.toml")
}

/// Load `config.toml` from the standard config directory.
/// Returns `AppConfig::default()` when the file is missing or unparseable.
pub fn load_config() -> AppConfig {
    let path = config_toml_path();
    if !path.exists() {
        return AppConfig::default();
    }

    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

/// Convenience: return the ks base_url if configured.
pub fn ks_base_url() -> Option<String> {
    load_config()
        .ks
        .and_then(|ks| ks.base_url)
        .filter(|u| !u.is_empty())
}

/// Convenience: return max bytes allowed for ks cached WebDAV downloads.
pub fn ks_download_max_total_bytes() -> Option<u64> {
    load_config().ks.and_then(|ks| ks.download_max_total_bytes)
}

/// Convenience: return ks cached-download directory override.
pub fn ks_download_cache_dir() -> Option<PathBuf> {
    load_config()
        .ks
        .and_then(|ks| ks.download_cache_dir)
        .map(PathBuf::from)
}
