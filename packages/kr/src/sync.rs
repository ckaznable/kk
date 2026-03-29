//! Synchronization helpers for communicating with the `ks` server.
//!
//! These use **blocking** HTTP so they can be called from the synchronous
//! `kk` GUI thread without pulling in a tokio runtime.

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct KsWebDavEnqueueRequest {
    pub url_path: String,
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub file_size: Option<u64>,
    #[serde(default)]
    pub source_base_url: Option<String>,
    #[serde(default)]
    pub source_user: Option<String>,
    #[serde(default)]
    pub source_pass: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct KsReadyDownload {
    pub id: String,
    pub url_path: String,
    pub file_name: String,
    pub size_bytes: u64,
}

fn kk_cache_path() -> std::path::PathBuf {
    dirs::DIR.config_local_dir().join("kk_cache.json")
}

/// Fetch `kr.json` content from the ks server and overwrite the local file.
/// Returns `true` if ks had data (local file overwritten),
/// or `false` if ks returned an empty/default response (caller should push local up).
pub fn pull_kr(base_url: &str) -> Result<bool> {
    let url = format!("{}/db/kr", base_url.trim_end_matches('/'));
    println!("[sync] Pulling kr.json from {}", url);
    let resp = reqwest::blocking::get(&url)?;
    if !resp.status().is_success() {
        anyhow::bail!("ks returned {} for GET /db/kr", resp.status());
    }
    let body = resp.text()?;
    // Treat "{}" or empty body as "ks has no data yet"
    if body.trim() == "{}" || body.trim().is_empty() {
        println!("[sync] ks returned empty kr.json, will push local.");
        return Ok(false);
    }
    let path = crate::db::SimpleJsonDatabase::config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, &body)?;
    println!("[sync] kr.json pulled ({} bytes)", body.len());
    Ok(true)
}

/// Push the local `kr.json` content to the ks server.
pub fn push_kr(base_url: &str) -> Result<()> {
    let path = crate::db::SimpleJsonDatabase::config_path();
    if !path.exists() {
        println!("[sync] No local kr.json to push.");
        return Ok(());
    }
    let body = std::fs::read_to_string(&path)?;
    let url = format!("{}/db/kr", base_url.trim_end_matches('/'));
    println!("[sync] Pushing kr.json to {}", url);
    let client = reqwest::blocking::Client::new();
    let resp = client
        .put(&url)
        .header("Content-Type", "application/json")
        .body(body.clone())
        .send()?;
    if !resp.status().is_success() {
        anyhow::bail!("ks returned {} for PUT /db/kr", resp.status());
    }
    println!("[sync] kr.json pushed ({} bytes)", body.len());
    Ok(())
}

/// Fetch `kwa_db.json` content from the ks server and overwrite the local file.
/// Returns `true` if ks had data (local file overwritten),
/// or `false` if ks returned an empty/default response (caller should push local up).
pub fn pull_kwa(base_url: &str) -> Result<bool> {
    let url = format!("{}/db/kwa", base_url.trim_end_matches('/'));
    println!("[sync] Pulling kwa_db.json from {}", url);
    let resp = reqwest::blocking::get(&url)?;
    if !resp.status().is_success() {
        anyhow::bail!("ks returned {} for GET /db/kwa", resp.status());
    }
    let body = resp.text()?;
    if body.trim() == "{}" || body.trim().is_empty() {
        println!("[sync] ks returned empty kwa_db.json, will push local.");
        return Ok(false);
    }
    let path = crate::db::WebDavDatabase::config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, &body)?;
    println!("[sync] kwa_db.json pulled ({} bytes)", body.len());
    Ok(true)
}

/// Push the local `kwa_db.json` content to the ks server.
pub fn push_kwa(base_url: &str) -> Result<()> {
    let path = crate::db::WebDavDatabase::config_path();
    if !path.exists() {
        println!("[sync] No local kwa_db.json to push.");
        return Ok(());
    }
    let body = std::fs::read_to_string(&path)?;
    let url = format!("{}/db/kwa", base_url.trim_end_matches('/'));
    println!("[sync] Pushing kwa_db.json to {}", url);
    let client = reqwest::blocking::Client::new();
    let resp = client
        .put(&url)
        .header("Content-Type", "application/json")
        .body(body.clone())
        .send()?;
    if !resp.status().is_success() {
        anyhow::bail!("ks returned {} for PUT /db/kwa", resp.status());
    }
    println!("[sync] kwa_db.json pushed ({} bytes)", body.len());
    Ok(())
}

/// Enqueue a WebDAV path on ks for background downloading.
pub fn enqueue_webdav_download(base_url: &str, req: &KsWebDavEnqueueRequest) -> Result<()> {
    let url = format!("{}/downloads/webdav", base_url.trim_end_matches('/'));
    let client = reqwest::blocking::Client::new();
    let resp = client.post(&url).json(req).send()?;
    if !resp.status().is_success() {
        anyhow::bail!("ks returned {} for POST /downloads/webdav", resp.status());
    }
    Ok(())
}

/// Fetch `kk_cache.json` content from the ks server and overwrite the local file.
/// Returns `true` if ks had data (local file overwritten),
/// or `false` if ks returned an empty/default response (caller should push local up).
pub fn pull_kk_cache(base_url: &str) -> Result<bool> {
    let url = format!("{}/db/kk-cache", base_url.trim_end_matches('/'));
    println!("[sync] Pulling kk_cache.json from {}", url);
    let resp = reqwest::blocking::get(&url)?;
    if !resp.status().is_success() {
        anyhow::bail!("ks returned {} for GET /db/kk-cache", resp.status());
    }
    let body = resp.text()?;
    // Treat "{}" or empty body as "ks has no data yet"
    if body.trim() == "{}" || body.trim().is_empty() {
        println!("[sync] ks returned empty kk_cache.json, will push local.");
        return Ok(false);
    }
    let path = kk_cache_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, &body)?;
    println!("[sync] kk_cache.json pulled ({} bytes)", body.len());
    Ok(true)
}

/// Push the local `kk_cache.json` content to the ks server.
pub fn push_kk_cache(base_url: &str) -> Result<()> {
    let path = kk_cache_path();
    if !path.exists() {
        println!("[sync] No local kk_cache.json to push.");
        return Ok(());
    }
    let body = std::fs::read_to_string(&path)?;
    let url = format!("{}/db/kk-cache", base_url.trim_end_matches('/'));
    println!("[sync] Pushing kk_cache.json to {}", url);
    let client = reqwest::blocking::Client::new();
    let resp = client
        .put(&url)
        .header("Content-Type", "application/json")
        .body(body.clone())
        .send()?;
    if !resp.status().is_success() {
        anyhow::bail!("ks returned {} for PUT /db/kk-cache", resp.status());
    }
    println!("[sync] kk_cache.json pushed ({} bytes)", body.len());
    Ok(())
}
