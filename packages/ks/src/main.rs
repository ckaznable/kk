use anyhow::{Context, Result};
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use percent_encoding::percent_decode_str;
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::{
    collections::HashMap,
    net::SocketAddr,
    path::{Path as FsPath, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex as AsyncMutex;
#[cfg(unix)]
use tokio_util::bytes;
use tokio_util::io::ReaderStream;
use tower_http::cors::CorsLayer;
use tracing::{error, info, warn};

// ── Server State ─────────────────────────────────────────────────────────────

#[derive(Clone)]
struct AppState {
    data_dir: PathBuf,
    cache: Arc<Mutex<kr::cache::KkCache>>,
    downloads: DownloadManager,
}

impl AppState {
    fn kr_path(&self) -> PathBuf {
        self.data_dir.join("kr.json")
    }

    fn kwa_path(&self) -> PathBuf {
        self.data_dir.join("kwa_db.json")
    }
}

// ── JSON response helpers ────────────────────────────────────────────────────

#[derive(Serialize)]
struct ApiOk {
    ok: bool,
}

#[derive(Serialize)]
struct ApiErr {
    ok: bool,
    error: String,
}

fn ok_json() -> Response {
    (StatusCode::OK, Json(ApiOk { ok: true })).into_response()
}

fn err_json(status: StatusCode, msg: impl ToString) -> Response {
    (
        status,
        Json(ApiErr {
            ok: false,
            error: msg.to_string(),
        }),
    )
        .into_response()
}

// ── Size guard helper ────────────────────────────────────────────────────────

/// Returns `true` if `incoming_len` is suspiciously smaller than the current
/// file on disk (less than 90 % of its size). When `true` the caller should
/// reject the request to avoid overwriting a larger database with a
/// truncated / stale payload.
async fn is_suspiciously_small(path: &PathBuf, incoming_len: usize) -> bool {
    if let Ok(meta) = tokio::fs::metadata(path).await {
        let current_size = meta.len() as usize;
        if current_size > 0 {
            // threshold: 90 % of the cached file size
            let threshold = current_size * 9 / 10;
            return incoming_len < threshold;
        }
    }
    false
}

// ── Download queue types ─────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct DownloadQueueFile {
    pending: Vec<QueuedDownload>,
    ready: Vec<ReadyDownload>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum PendingStatus {
    Queued,
    #[serde(alias = "running")]
    Downloading,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct QueuedDownload {
    id: String,
    url_path: String,
    file_name: String,
    file_size: Option<u64>,
    source_base_url: String,
    source_user: Option<String>,
    source_pass: Option<String>,
    status: PendingStatus,
    enqueued_at: u64,
    #[serde(default)]
    retry_count: u32,
}

const MAX_DOWNLOAD_RETRIES: u32 = 5;
const KK_NOTIFY_URL: &str = "http://ntfy-service.ntfy.svc.cluster.local/kk";

#[derive(Serialize, Deserialize, Clone, Debug)]
struct ReadyDownload {
    id: String,
    url_path: String,
    file_name: String,
    #[serde(default)]
    stored_path: String,
    // Backward compatibility: old queue snapshots stored `stored_name` only.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    stored_name: String,
    size_bytes: u64,
    finished_at: u64,
}

#[derive(Serialize)]
struct PendingDownloadView {
    id: String,
    url_path: String,
    file_name: String,
    file_size: Option<u64>,
    status: PendingStatus,
    enqueued_at: u64,
}

#[derive(Serialize)]
struct DownloadQueueView {
    pending: Vec<PendingDownloadView>,
    ready_count: usize,
    cache_bytes: u64,
    max_cache_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct DownloadCounts {
    pending_total: usize,
    queued: usize,
    downloading: usize,
    ready: usize,
}

#[derive(Clone)]
struct DownloadManager {
    state: Arc<AsyncMutex<DownloadQueueFile>>,
    queue_path: PathBuf,
    cache_dir: PathBuf,
    download_dir: PathBuf,
    source_prefixes: Vec<String>,
    max_total_bytes: u64,
}

impl DownloadManager {
    async fn load(
        queue_path: PathBuf,
        cache_dir: PathBuf,
        download_dir: PathBuf,
        source_prefixes: Vec<String>,
        max_total_bytes: u64,
    ) -> Result<Self> {
        if let Some(parent) = queue_path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        tokio::fs::create_dir_all(&cache_dir).await.ok();
        tokio::fs::create_dir_all(&download_dir).await.ok();

        let mut queue: DownloadQueueFile = match tokio::fs::read_to_string(&queue_path).await {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => DownloadQueueFile::default(),
        };

        // If ks restarted while a task was downloading, make it queueable again.
        for item in &mut queue.pending {
            if item.status == PendingStatus::Downloading {
                item.status = PendingStatus::Queued;
            }
        }

        let manager = Self {
            state: Arc::new(AsyncMutex::new(queue.clone())),
            queue_path,
            cache_dir,
            download_dir,
            source_prefixes,
            max_total_bytes,
        };

        manager.persist_snapshot(&queue).await?;
        Ok(manager)
    }

    async fn persist_snapshot(&self, snapshot: &DownloadQueueFile) -> Result<()> {
        if let Some(parent) = self.queue_path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        let body = serde_json::to_string_pretty(snapshot)?;
        tokio::fs::write(&self.queue_path, body).await?;
        Ok(())
    }

    async fn enqueue(&self, req: kr::sync::KsWebDavEnqueueRequest) -> Result<(String, bool)> {
        let source_base_url = req
            .source_base_url
            .or_else(|| dirs::WEBDAV_URL.clone())
            .filter(|s| !s.is_empty())
            .context("Missing WebDAV source base URL for queued download")?;

        let file_name = req
            .file_name
            .filter(|s| !s.is_empty())
            .and_then(|name| {
                FsPath::new(&name)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
            })
            .or_else(|| {
                FsPath::new(&req.url_path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
            })
            .map(|name| sanitize_filename(&name))
            .unwrap_or_else(|| "download.bin".to_string());
        let source_user = req.source_user.or_else(|| dirs::WEBDAV_USER.clone());
        let source_pass = req.source_pass.or_else(|| dirs::WEBDAV_PASS.clone());

        let mut state = self.state.lock().await;

        if let Some(existing) = state.pending.iter().find(|d| d.url_path == req.url_path) {
            return Ok((existing.id.clone(), false));
        }

        if let Some(existing) = state.ready.iter().find(|d| d.url_path == req.url_path) {
            return Ok((existing.id.clone(), false));
        }

        let now = now_ts();
        let id = format!("{}-{}", now, state.pending.len() + state.ready.len() + 1);
        state.pending.push(QueuedDownload {
            id: id.clone(),
            url_path: req.url_path,
            file_name,
            file_size: req.file_size,
            source_base_url,
            source_user,
            source_pass,
            status: PendingStatus::Queued,
            enqueued_at: now,
            retry_count: 0,
        });

        let snapshot = state.clone();
        drop(state);
        self.persist_snapshot(&snapshot).await?;

        Ok((id, true))
    }

    async fn pop_next_queued_mark_downloading(&self) -> Result<Option<QueuedDownload>> {
        let mut state = self.state.lock().await;
        let idx = match state
            .pending
            .iter()
            .position(|d| d.status == PendingStatus::Queued)
        {
            Some(i) => i,
            None => return Ok(None),
        };

        state.pending[idx].status = PendingStatus::Downloading;
        let item = state.pending[idx].clone();

        let snapshot = state.clone();
        drop(state);
        self.persist_snapshot(&snapshot).await?;

        Ok(Some(item))
    }

    async fn set_queued(&self, id: &str) -> Result<()> {
        let mut state = self.state.lock().await;
        if let Some(item) = state.pending.iter_mut().find(|d| d.id == id) {
            item.status = PendingStatus::Queued;
            let snapshot = state.clone();
            drop(state);
            self.persist_snapshot(&snapshot).await?;
        }
        Ok(())
    }

    /// Increment retry count, set back to queued, and return the new count.
    async fn increment_retry_count(&self, id: &str) -> Result<u32> {
        let mut state = self.state.lock().await;
        if let Some(item) = state.pending.iter_mut().find(|d| d.id == id) {
            item.retry_count += 1;
            item.status = PendingStatus::Queued;
            let count = item.retry_count;
            let snapshot = state.clone();
            drop(state);
            self.persist_snapshot(&snapshot).await?;
            Ok(count)
        } else {
            Ok(0)
        }
    }

    /// Remove a pending download entirely (used after max retries exceeded).
    async fn remove_pending(&self, id: &str) -> Result<()> {
        let mut state = self.state.lock().await;
        state.pending.retain(|d| d.id != id);
        let snapshot = state.clone();
        drop(state);
        self.persist_snapshot(&snapshot).await?;
        Ok(())
    }

    async fn mark_ready(&self, id: &str, stored_path: PathBuf, size_bytes: u64) -> Result<()> {
        let mut state = self.state.lock().await;
        let Some(pos) = state.pending.iter().position(|d| d.id == id) else {
            return Ok(());
        };

        let queued = state.pending.remove(pos);
        state.ready.push(ReadyDownload {
            id: queued.id,
            url_path: queued.url_path,
            file_name: queued.file_name,
            stored_path: stored_path.to_string_lossy().to_string(),
            stored_name: String::new(),
            size_bytes,
            finished_at: now_ts(),
        });

        let snapshot = state.clone();
        drop(state);
        self.persist_snapshot(&snapshot).await?;

        Ok(())
    }

    async fn ready_list(&self) -> Vec<kr::sync::KsReadyDownload> {
        let state = self.state.lock().await;
        state
            .ready
            .iter()
            .map(|r| kr::sync::KsReadyDownload {
                id: r.id.clone(),
                url_path: r.url_path.clone(),
                file_name: r.file_name.clone(),
                size_bytes: r.size_bytes,
            })
            .collect()
    }

    async fn ready_file_info(&self, id: &str) -> Option<(PathBuf, String)> {
        let state = self.state.lock().await;
        state.ready.iter().find(|r| r.id == id).map(|r| {
            let path = if !r.stored_path.is_empty() {
                PathBuf::from(&r.stored_path)
            } else {
                self.cache_dir.join(&r.stored_name)
            };
            (path, r.file_name.clone())
        })
    }

    async fn delete_ready(&self, id: &str) -> Result<bool> {
        let mut state = self.state.lock().await;
        let Some(pos) = state.ready.iter().position(|r| r.id == id) else {
            return Ok(false);
        };

        let ready = state.ready.remove(pos);
        let snapshot = state.clone();
        drop(state);

        self.persist_snapshot(&snapshot).await?;

        let f = if !ready.stored_path.is_empty() {
            PathBuf::from(ready.stored_path)
        } else {
            self.cache_dir.join(ready.stored_name)
        };
        if let Err(e) = tokio::fs::remove_file(&f).await {
            if e.kind() != std::io::ErrorKind::NotFound {
                warn!(error = %e, path = %f.display(), "Failed to remove cached file");
            }
        }

        Ok(true)
    }

    async fn queue_view(&self) -> DownloadQueueView {
        let state = self.state.lock().await;
        let pending = state
            .pending
            .iter()
            .map(|d| PendingDownloadView {
                id: d.id.clone(),
                url_path: d.url_path.clone(),
                file_name: d.file_name.clone(),
                file_size: d.file_size,
                status: d.status.clone(),
                enqueued_at: d.enqueued_at,
            })
            .collect::<Vec<_>>();
        let ready_count = state.ready.len();
        drop(state);

        DownloadQueueView {
            pending,
            ready_count,
            cache_bytes: self.total_cache_size().await,
            max_cache_bytes: self.max_total_bytes,
        }
    }

    async fn counts(&self) -> DownloadCounts {
        let state = self.state.lock().await;
        let queued = state
            .pending
            .iter()
            .filter(|d| d.status == PendingStatus::Queued)
            .count();
        let downloading = state
            .pending
            .iter()
            .filter(|d| d.status == PendingStatus::Downloading)
            .count();

        DownloadCounts {
            pending_total: state.pending.len(),
            queued,
            downloading,
            ready: state.ready.len(),
        }
    }

    async fn total_cache_size(&self) -> u64 {
        let dir = self.download_dir.clone();
        tokio::task::spawn_blocking(move || dir_size_bytes(&dir))
            .await
            .unwrap_or(0)
    }
}

fn now_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_secs()
}

fn format_mib_per_sec(bytes: u64, elapsed: Duration) -> String {
    let secs = elapsed.as_secs_f64();
    if secs <= f64::EPSILON {
        return "n/a".to_string();
    }
    format!("{:.2}", bytes as f64 / (1024.0 * 1024.0) / secs)
}

const STREAM_PROGRESS_STEP_PCT: u64 = 5;

fn sanitize_filename(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        let bad = matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|');
        if bad {
            out.push('_');
        } else {
            out.push(ch);
        }
    }
    if out.is_empty() {
        "download.bin".to_string()
    } else {
        out
    }
}

fn normalize_webdav_prefix(prefix: &str) -> String {
    let mut p = prefix.trim().replace('\\', "/");
    if p.is_empty() {
        return "/".to_string();
    }
    if !p.starts_with('/') {
        p.insert(0, '/');
    }
    while p.ends_with('/') && p.len() > 1 {
        p.pop();
    }
    p
}

fn build_webdav_candidate_paths(original: &str, source_prefixes: &[String]) -> Vec<String> {
    let mut raw_candidates = Vec::new();

    let normalized_original = original.trim().replace('\\', "/");
    let original_relative = normalized_original.trim_start_matches('/');
    let remaining_subpath = original_relative
        .split_once('/')
        .map(|(_, rest)| rest)
        .filter(|rest| !rest.is_empty());

    let file_name = FsPath::new(original)
        .file_name()
        .map(|n| n.to_string_lossy().to_string());

    // Prefix-based paths first – replace only the first directory segment and
    // preserve the rest of the original path whenever possible.
    if let Some(remaining_subpath) = remaining_subpath {
        for prefix in source_prefixes {
            let base = normalize_webdav_prefix(prefix);
            let candidate = if base == "/" {
                format!("/{}", remaining_subpath)
            } else {
                format!("{}/{}", base, remaining_subpath)
            };
            if !raw_candidates.iter().any(|p| p == &candidate) {
                raw_candidates.push(candidate);
            }
        }
    } else if let Some(ref file_name) = file_name {
        for prefix in source_prefixes {
            let base = normalize_webdav_prefix(prefix);
            let candidate = if base == "/" {
                format!("/{}", file_name)
            } else {
                format!("{}/{}", base, file_name)
            };
            if !raw_candidates.iter().any(|p| p == &candidate) {
                raw_candidates.push(candidate);
            }
        }
    }

    // Original path as fallback
    if !original.trim().is_empty() && !raw_candidates.iter().any(|p| p == original) {
        raw_candidates.push(original.to_string());
    }

    let mut out = Vec::new();
    for candidate in raw_candidates {
        if !out.iter().any(|p| p == &candidate) {
            out.push(candidate.clone());
        }
        if let Some(decoded) = decode_webdav_candidate_path(&candidate)
            && !out.iter().any(|p| p == &decoded)
        {
            out.push(decoded);
        }
    }

    out
}

fn decode_webdav_candidate_path(path: &str) -> Option<String> {
    let decoded = percent_decode_str(path).decode_utf8().ok()?.into_owned();
    (decoded != path).then_some(decoded)
}

fn build_webdav_attempt_url(base_url: &str, path: &str) -> String {
    reqwest::Url::parse(base_url)
        .and_then(|url| url.join(path))
        .map(|url| url.to_string())
        .unwrap_or_else(|_| format!("{} :: {}", base_url, path))
}

fn dir_size_bytes(path: &FsPath) -> u64 {
    let mut total = 0u64;
    let Ok(rd) = std::fs::read_dir(path) else {
        return 0;
    };

    for ent in rd.flatten() {
        let p = ent.path();
        if let Ok(meta) = ent.metadata() {
            if meta.is_file() {
                total = total.saturating_add(meta.len());
            } else if meta.is_dir() {
                total = total.saturating_add(dir_size_bytes(&p));
            }
        }
    }

    total
}

const MIN_READY_DOWNLOAD_BYTES: u64 = 200 * 1024 * 1024; // 200 MiB

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct WebDavClientKey {
    base_url: String,
    source_user: Option<String>,
    source_pass: Option<String>,
}

async fn send_ks_notification(
    body: String,
    success_message: &'static str,
    failure_message: &'static str,
) {
    let http = reqwest::Client::new();
    if let Err(e) = http.post(KK_NOTIFY_URL).body(body).send().await {
        warn!(error = %e, url = KK_NOTIFY_URL, "{failure_message}");
    } else {
        info!(url = KK_NOTIFY_URL, "{success_message}");
    }
}

async fn download_worker_loop(manager: DownloadManager) {
    let mut client_pool: HashMap<WebDavClientKey, kwa::WebDavClient> = HashMap::new();

    loop {
        let next = match manager.pop_next_queued_mark_downloading().await {
            Ok(Some(task)) => task,
            Ok(None) => {
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
            Err(e) => {
                error!(error = %e, "Queue pop failed");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };

        let current_bytes = manager.total_cache_size().await;
        if current_bytes >= manager.max_total_bytes {
            info!(
                id = %next.id,
                cache_bytes = current_bytes,
                max_bytes = manager.max_total_bytes,
                "Download queue paused: cache size already reached limit"
            );
            if let Err(e) = manager.set_queued(&next.id).await {
                error!(error = %e, id = %next.id, "Failed to reset queued status");
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }

        if let Some(task_size) = next.file_size {
            if current_bytes.saturating_add(task_size) > manager.max_total_bytes {
                info!(
                    id = %next.id,
                    cache_bytes = current_bytes,
                    task_bytes = task_size,
                    max_bytes = manager.max_total_bytes,
                    "Download queue paused: adding this file would exceed limit"
                );
                if let Err(e) = manager.set_queued(&next.id).await {
                    error!(error = %e, id = %next.id, "Failed to reset queued status");
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        }

        let key = WebDavClientKey {
            base_url: next.source_base_url.clone(),
            source_user: next.source_user.clone(),
            source_pass: next.source_pass.clone(),
        };

        let client = if let Some(existing) = client_pool.get(&key) {
            existing.clone()
        } else {
            match kwa::WebDavClient::new(
                &key.base_url,
                key.source_user.clone().zip(key.source_pass.clone()),
            ) {
                Ok(c) => {
                    client_pool.insert(key, c.clone());
                    c
                }
                Err(e) => {
                    error!(error = %e, id = %next.id, "Failed to create WebDAV client");
                    if let Err(e) = manager.set_queued(&next.id).await {
                        error!(error = %e, id = %next.id, "Failed to reset queued status");
                    }
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            }
        };

        let dest = manager.download_dir.join(&next.file_name);
        let candidate_paths =
            build_webdav_candidate_paths(&next.url_path, &manager.source_prefixes);
        if candidate_paths.is_empty() {
            error!(id = %next.id, path = %next.url_path, "No valid WebDAV source path to try");
            if let Err(e) = manager.set_queued(&next.id).await {
                error!(error = %e, id = %next.id, "Failed to reset queued status");
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
            continue;
        }

        info!(
            id = %next.id,
            path = %next.url_path,
            file_name = %next.file_name,
            dest = %dest.display(),
            source_base_url = %next.source_base_url,
            source_prefixes = ?manager.source_prefixes,
            candidate_count = candidate_paths.len(),
            candidates = ?candidate_paths,
            "Starting WebDAV background download"
        );

        let mut downloaded: Option<(String, u64)> = None;
        let mut last_error: Option<String> = None;
        for (attempt_idx, path) in candidate_paths.iter().enumerate() {
            let remote_url = build_webdav_attempt_url(&next.source_base_url, path);
            info!(
                id = %next.id,
                attempt = attempt_idx + 1,
                total_attempts = candidate_paths.len(),
                original_path = %next.url_path,
                candidate_path = %path,
                remote_url = %remote_url,
                dest = %dest.display(),
                "Attempting WebDAV download candidate"
            );

            match client.download(path, &dest).await {
                Ok(()) => {
                    let size = match tokio::fs::metadata(&dest).await {
                        Ok(m) => m.len(),
                        Err(e) => {
                            warn!(
                                error = %e,
                                id = %next.id,
                                attempt = attempt_idx + 1,
                                total_attempts = candidate_paths.len(),
                                path = %path,
                                remote_url = %remote_url,
                                dest = %dest.display(),
                                "Downloaded file metadata check failed"
                            );
                            last_error = Some(format!("metadata check failed: {}", e));
                            continue;
                        }
                    };

                    if size < MIN_READY_DOWNLOAD_BYTES {
                        warn!(
                            id = %next.id,
                            attempt = attempt_idx + 1,
                            total_attempts = candidate_paths.len(),
                            path = %path,
                            remote_url = %remote_url,
                            dest = %dest.display(),
                            bytes = size,
                            min_bytes = MIN_READY_DOWNLOAD_BYTES,
                            "WebDAV download rejected: file smaller than minimum size"
                        );
                        if let Err(e) = tokio::fs::remove_file(&dest).await
                            && e.kind() != std::io::ErrorKind::NotFound
                        {
                            warn!(
                                error = %e,
                                id = %next.id,
                                path = %dest.display(),
                                "Failed to remove too-small downloaded file"
                            );
                        }
                        last_error = Some(format!(
                            "downloaded file too small: {} bytes < {} bytes",
                            size, MIN_READY_DOWNLOAD_BYTES
                        ));
                        continue;
                    }

                    downloaded = Some((path.clone(), size));
                    info!(
                        id = %next.id,
                        attempt = attempt_idx + 1,
                        total_attempts = candidate_paths.len(),
                        path = %path,
                        remote_url = %remote_url,
                        dest = %dest.display(),
                        bytes = size,
                        "WebDAV download candidate succeeded"
                    );
                    break;
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        id = %next.id,
                        attempt = attempt_idx + 1,
                        total_attempts = candidate_paths.len(),
                        original_path = %next.url_path,
                        path = %path,
                        remote_url = %remote_url,
                        dest = %dest.display(),
                        "WebDAV download attempt failed"
                    );
                    last_error = Some(e.to_string());
                }
            }
        }

        if let Some((remote_path, size)) = downloaded {
            if let Err(e) = manager.mark_ready(&next.id, dest.clone(), size).await {
                error!(error = %e, id = %next.id, "Failed to mark task as ready");
            } else {
                let counts = manager.counts().await;
                info!(
                    id = %next.id,
                    path = %remote_path,
                    remote_url = %build_webdav_attempt_url(&next.source_base_url, &remote_path),
                    dest = %dest.display(),
                    bytes = size,
                    remaining_pending = counts.pending_total,
                    remaining_queued = counts.queued,
                    remaining_downloading = counts.downloading,
                    ready_count = counts.ready,
                    "WebDAV download completed"
                );

                if counts.pending_total == 0 {
                    let notify_body = format!(
                        "[ks] Download queue completed\nLast file: {}\nPath: {}\nBytes: {}\nReady files: {}",
                        next.file_name, remote_path, size, counts.ready
                    );
                    send_ks_notification(
                        notify_body,
                        "Completion notification sent",
                        "Failed to send completion notification",
                    )
                    .await;
                }
            }
        } else {
            let err = last_error.unwrap_or_else(|| "unknown error".to_string());
            error!(
                id = %next.id,
                path = %next.url_path,
                error = %err,
                dest = %dest.display(),
                source_base_url = %next.source_base_url,
                source_prefixes = ?manager.source_prefixes,
                tried_paths = ?candidate_paths,
                "WebDAV download failed"
            );

            // Increment retry count; if exceeded max retries, remove and notify
            match manager.increment_retry_count(&next.id).await {
                Ok(count) if count >= MAX_DOWNLOAD_RETRIES => {
                    error!(
                        id = %next.id,
                        path = %next.url_path,
                        retries = count,
                        "Max retries exceeded, removing from queue"
                    );
                    if let Err(e) = manager.remove_pending(&next.id).await {
                        error!(error = %e, id = %next.id, "Failed to remove pending download");
                    }

                    // Send failure notification
                    let notify_body = format!(
                        "[ks] Download failed after {} retries\nID: {}\nPath: {}\nFile: {}\nError: {}",
                        count, next.id, next.url_path, next.file_name, err
                    );
                    send_ks_notification(
                        notify_body,
                        "Failure notification sent",
                        "Failed to send failure notification",
                    )
                    .await;
                }
                Ok(count) => {
                    warn!(
                        id = %next.id,
                        retry = count,
                        max = MAX_DOWNLOAD_RETRIES,
                        "Will retry download later"
                    );
                }
                Err(e) => {
                    error!(error = %e, id = %next.id, "Failed to increment retry count");
                }
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    }
}

// ── Handlers: kr.json ────────────────────────────────────────────────────────

async fn stream_json_file_or_default(path: PathBuf, label: &'static str) -> Response {
    match tokio::fs::File::open(&path).await {
        Ok(file) => {
            let file_len = file.metadata().await.ok().map(|m| m.len());
            info!(
                file = label,
                path = %path.display(),
                bytes = file_len.unwrap_or(0),
                "Streaming JSON file"
            );

            let stream = ReaderStream::new(file);
            let mut resp = Response::new(Body::from_stream(stream));
            *resp.status_mut() = StatusCode::OK;
            resp.headers_mut().insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("application/json"),
            );
            if let Some(file_len) = file_len
                && let Ok(v) = header::HeaderValue::from_str(&file_len.to_string())
            {
                resp.headers_mut().insert(header::CONTENT_LENGTH, v);
            }
            resp
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            "{}".to_string(),
        )
            .into_response(),
        Err(e) => {
            error!(
                error = %e,
                file = label,
                path = %path.display(),
                "Failed to open JSON file"
            );
            err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        }
    }
}

async fn get_kr(State(state): State<AppState>) -> Response {
    stream_json_file_or_default(state.kr_path(), "kr.json").await
}

async fn put_kr(State(state): State<AppState>, body: String) -> Response {
    let path = state.kr_path();

    // Validate JSON
    if serde_json::from_str::<serde_json::Value>(&body).is_err() {
        return err_json(StatusCode::BAD_REQUEST, "Invalid JSON");
    }

    // Size guard: reject if incoming payload is < 90 % of the cached file
    if is_suspiciously_small(&path, body.len()).await {
        let current = tokio::fs::metadata(&path)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        warn!(
            incoming_bytes = body.len(),
            current_bytes = current,
            file = "kr.json",
            "Rejected PUT – incoming payload is less than 90 % of cached size"
        );
        return err_json(
            StatusCode::CONFLICT,
            "Rejected: incoming payload is suspiciously smaller than the cached database",
        );
    }

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }

    match tokio::fs::write(&path, &body).await {
        Ok(_) => {
            info!(bytes = body.len(), file = "kr.json", "Database updated");
            ok_json()
        }
        Err(e) => {
            error!(error = %e, file = "kr.json", "Failed to write database");
            err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        }
    }
}

// ── Handlers: kwa_db.json ────────────────────────────────────────────────────

async fn get_kwa(State(state): State<AppState>) -> Response {
    stream_json_file_or_default(state.kwa_path(), "kwa_db.json").await
}

async fn put_kwa(State(state): State<AppState>, body: String) -> Response {
    let path = state.kwa_path();

    if serde_json::from_str::<serde_json::Value>(&body).is_err() {
        return err_json(StatusCode::BAD_REQUEST, "Invalid JSON");
    }

    // Size guard: reject if incoming payload is < 90 % of the cached file
    if is_suspiciously_small(&path, body.len()).await {
        let current = tokio::fs::metadata(&path)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        warn!(
            incoming_bytes = body.len(),
            current_bytes = current,
            file = "kwa_db.json",
            "Rejected PUT – incoming payload is less than 90 % of cached size"
        );
        return err_json(
            StatusCode::CONFLICT,
            "Rejected: incoming payload is suspiciously smaller than the cached database",
        );
    }

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }

    match tokio::fs::write(&path, &body).await {
        Ok(_) => {
            info!(bytes = body.len(), file = "kwa_db.json", "Database updated");
            ok_json()
        }
        Err(e) => {
            error!(error = %e, file = "kwa_db.json", "Failed to write database");
            err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        }
    }
}

// ── Handlers: kk_cache.json ──────────────────────────────────────────────────

async fn get_kk_cache(State(state): State<AppState>) -> Response {
    let cache = match state.cache.lock() {
        Ok(cache) => cache,
        Err(e) => {
            error!(error = %e, "Failed to lock kk cache");
            return err_json(StatusCode::INTERNAL_SERVER_ERROR, "Failed to lock kk cache");
        }
    };

    match serde_json::to_string(&*cache) {
        Ok(body) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            body,
        )
            .into_response(),
        Err(e) => {
            error!(error = %e, "Failed to serialize kk cache");
            err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        }
    }
}

async fn put_kk_cache(State(state): State<AppState>, body: String) -> Response {
    let parsed = match serde_json::from_str::<kr::cache::KkCache>(&body) {
        Ok(v) => v,
        Err(_) => return err_json(StatusCode::BAD_REQUEST, "Invalid kk_cache JSON"),
    };

    let mut cache = match state.cache.lock() {
        Ok(cache) => cache,
        Err(e) => {
            error!(error = %e, "Failed to lock kk cache");
            return err_json(StatusCode::INTERNAL_SERVER_ERROR, "Failed to lock kk cache");
        }
    };

    *cache = parsed;
    cache.flush();
    info!(bytes = body.len(), file = "kk_cache.json", "Cache updated");
    ok_json()
}

// ── Handlers: WebDAV downloads ───────────────────────────────────────────────

#[derive(Serialize)]
struct EnqueueResp {
    ok: bool,
    id: String,
    queued: bool,
}

async fn post_download_webdav(
    State(state): State<AppState>,
    Json(req): Json<kr::sync::KsWebDavEnqueueRequest>,
) -> Response {
    match state.downloads.enqueue(req).await {
        Ok((id, queued)) => (
            StatusCode::OK,
            Json(EnqueueResp {
                ok: true,
                id,
                queued,
            }),
        )
            .into_response(),
        Err(e) => err_json(StatusCode::BAD_REQUEST, e.to_string()),
    }
}

async fn get_download_webdav(State(state): State<AppState>) -> Response {
    let view = state.downloads.queue_view().await;
    (StatusCode::OK, Json(view)).into_response()
}

async fn get_download_webdav_ready(State(state): State<AppState>) -> Response {
    let ready = state.downloads.ready_list().await;
    (StatusCode::OK, Json(ready)).into_response()
}

/// Stream a file with periodic `posix_fadvise(DONTNEED)` hints to prevent
/// page-cache buildup in cgroup-constrained containers (k3s / Kubernetes).
///
/// Uses tokio's native async file I/O for maximum throughput. fadvise is
/// called every `FADVISE_INTERVAL` bytes as a fire-and-forget background
/// task so it never blocks the stream.
#[cfg(unix)]
fn file_stream_with_fadvise(
    file: tokio::fs::File,
    id: String,
    file_name: String,
    path: PathBuf,
    expected_bytes: Option<u64>,
) -> impl futures_util::stream::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send {
    use futures_util::StreamExt;

    let fd = file.as_raw_fd();
    // 2 MB read buffer – large enough for fast LAN transfers without
    // excessive memory pinning.
    const BUF_SIZE: usize = 2 * 1024 * 1024;
    // Advise the kernel every 32 MB; fine-grained enough to keep page
    // cache from growing too large, coarse-grained enough to be cheap.
    const FADVISE_INTERVAL: i64 = 32 * 1024 * 1024;

    let inner = ReaderStream::with_capacity(file, BUF_SIZE);
    let started_at = Instant::now();

    futures_util::stream::unfold(
        (inner, 0i64, 0i64, started_at, STREAM_PROGRESS_STEP_PCT),
        move |(mut inner, mut bytes_read, mut last_advised, started_at, mut next_progress_pct)| {
            let id = id.clone();
            let file_name = file_name.clone();
            let path = path.clone();

            async move {
                match inner.next().await {
                    Some(result) => {
                        match &result {
                            Ok(chunk) => {
                                bytes_read += chunk.len() as i64;
                                if let Some(expected_bytes) = expected_bytes
                                    && expected_bytes > 0
                                {
                                    let streamed_bytes = bytes_read.max(0) as u64;
                                    let pct =
                                        (streamed_bytes.saturating_mul(100) / expected_bytes).min(100);
                                    while pct >= next_progress_pct {
                                        let elapsed = started_at.elapsed();
                                        info!(
                                            id = %id,
                                            file_name = %file_name,
                                            path = %path.display(),
                                            progress_pct = next_progress_pct,
                                            streamed_bytes,
                                            expected_bytes,
                                            elapsed_secs = elapsed.as_secs_f64(),
                                            avg_mib_per_sec = %format_mib_per_sec(streamed_bytes, elapsed),
                                            "Ready file stream progress"
                                        );
                                        next_progress_pct += STREAM_PROGRESS_STEP_PCT;
                                    }
                                }
                                if bytes_read - last_advised >= FADVISE_INTERVAL {
                                    let start = last_advised;
                                    let len = bytes_read - last_advised;
                                    last_advised = bytes_read;
                                    // fadvise is a fast hint syscall; run it without awaiting
                                    // so the stream is never stalled waiting for it.
                                    tokio::task::spawn_blocking(move || unsafe {
                                        libc::posix_fadvise(
                                            fd,
                                            start,
                                            len as libc::off_t,
                                            libc::POSIX_FADV_DONTNEED,
                                        );
                                    });
                                }
                            }
                            Err(err) => {
                                let elapsed = started_at.elapsed();
                                warn!(
                                    id = %id,
                                    file_name = %file_name,
                                    path = %path.display(),
                                    expected_bytes = expected_bytes.unwrap_or(0),
                                    streamed_bytes = bytes_read.max(0) as u64,
                                    elapsed_secs = elapsed.as_secs_f64(),
                                    avg_mib_per_sec = %format_mib_per_sec(bytes_read.max(0) as u64, elapsed),
                                    error = %err,
                                    "Ready file stream failed"
                                );
                            }
                        }

                        Some((result, (inner, bytes_read, last_advised, started_at, next_progress_pct)))
                    }
                    None => {
                        let streamed_bytes = bytes_read.max(0) as u64;
                        let elapsed = started_at.elapsed();
                        info!(
                            id = %id,
                            file_name = %file_name,
                            path = %path.display(),
                            expected_bytes = expected_bytes.unwrap_or(streamed_bytes),
                            streamed_bytes,
                            elapsed_secs = elapsed.as_secs_f64(),
                            avg_mib_per_sec = %format_mib_per_sec(streamed_bytes, elapsed),
                            "Completed ready file stream"
                        );
                        None
                    }
                }
            }
        },
    )
}

#[cfg(not(unix))]
fn file_stream_plain(
    file: tokio::fs::File,
    id: String,
    file_name: String,
    path: PathBuf,
    expected_bytes: Option<u64>,
) -> impl futures_util::stream::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send {
    use futures_util::StreamExt;

    const BUF_SIZE: usize = 2 * 1024 * 1024;
    let inner = ReaderStream::with_capacity(file, BUF_SIZE);
    let started_at = Instant::now();

    futures_util::stream::unfold(
        (inner, 0u64, started_at, STREAM_PROGRESS_STEP_PCT),
        move |(mut inner, bytes_read, started_at, mut next_progress_pct)| {
            let id = id.clone();
            let file_name = file_name.clone();
            let path = path.clone();

            async move {
                match inner.next().await {
                    Some(result) => {
                        let next_bytes_read = match &result {
                            Ok(chunk) => {
                                let next_bytes_read = bytes_read.saturating_add(chunk.len() as u64);
                                if let Some(expected_bytes) = expected_bytes
                                    && expected_bytes > 0
                                {
                                    let pct =
                                        (next_bytes_read.saturating_mul(100) / expected_bytes).min(100);
                                    while pct >= next_progress_pct {
                                        let elapsed = started_at.elapsed();
                                        info!(
                                            id = %id,
                                            file_name = %file_name,
                                            path = %path.display(),
                                            progress_pct = next_progress_pct,
                                            streamed_bytes = next_bytes_read,
                                            expected_bytes,
                                            elapsed_secs = elapsed.as_secs_f64(),
                                            avg_mib_per_sec = %format_mib_per_sec(next_bytes_read, elapsed),
                                            "Ready file stream progress"
                                        );
                                        next_progress_pct += STREAM_PROGRESS_STEP_PCT;
                                    }
                                }
                                next_bytes_read
                            }
                            Err(err) => {
                                let elapsed = started_at.elapsed();
                                warn!(
                                    id = %id,
                                    file_name = %file_name,
                                    path = %path.display(),
                                    expected_bytes = expected_bytes.unwrap_or(0),
                                    streamed_bytes = bytes_read,
                                    elapsed_secs = elapsed.as_secs_f64(),
                                    avg_mib_per_sec = %format_mib_per_sec(bytes_read, elapsed),
                                    error = %err,
                                    "Ready file stream failed"
                                );
                                bytes_read
                            }
                        };

                        Some((result, (inner, next_bytes_read, started_at, next_progress_pct)))
                    }
                    None => {
                        let elapsed = started_at.elapsed();
                        info!(
                            id = %id,
                            file_name = %file_name,
                            path = %path.display(),
                            expected_bytes = expected_bytes.unwrap_or(bytes_read),
                            streamed_bytes = bytes_read,
                            elapsed_secs = elapsed.as_secs_f64(),
                            avg_mib_per_sec = %format_mib_per_sec(bytes_read, elapsed),
                            "Completed ready file stream"
                        );
                        None
                    }
                }
            }
        },
    )
}

async fn get_download_webdav_ready_file(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let Some((path, file_name)) = state.downloads.ready_file_info(&id).await else {
        return err_json(StatusCode::NOT_FOUND, "Ready download not found");
    };

    match tokio::fs::File::open(&path).await {
        Ok(file) => {
            let file_len = file.metadata().await.ok().map(|m| m.len());
            info!(
                id = %id,
                file_name = %file_name,
                path = %path.display(),
                bytes = file_len.unwrap_or(0),
                "Streaming ready file"
            );

            // On Unix, use fadvise(DONTNEED) to prevent page-cache OOM.
            // On other platforms, fall back to plain ReaderStream.
            #[cfg(unix)]
            let body = Body::from_stream(file_stream_with_fadvise(
                file,
                id.clone(),
                file_name.clone(),
                path.clone(),
                file_len,
            ));
            #[cfg(not(unix))]
            let body = Body::from_stream(file_stream_plain(
                file,
                id.clone(),
                file_name.clone(),
                path.clone(),
                file_len,
            ));

            let mut resp = Response::new(body);
            *resp.status_mut() = StatusCode::OK;
            resp.headers_mut().insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("application/octet-stream"),
            );
            if let Some(file_len) = file_len
                && let Ok(v) = header::HeaderValue::from_str(&file_len.to_string())
            {
                resp.headers_mut().insert(header::CONTENT_LENGTH, v);
            }
            if let Ok(v) = header::HeaderValue::from_str(&format!(
                "attachment; filename=\"{}\"",
                file_name.replace('"', "_")
            )) {
                resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
            }
            resp
        }
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                warn!(
                    id = %id,
                    path = %path.display(),
                    "Ready file missing on disk"
                );
                err_json(StatusCode::NOT_FOUND, "Ready file missing on server")
            } else {
                error!(error = %e, path = %path.display(), "Failed to open ready file");
                err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            }
        }
    }
}

async fn delete_download_webdav_ready(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    match state.downloads.delete_ready(&id).await {
        Ok(true) => ok_json(),
        Ok(false) => err_json(StatusCode::NOT_FOUND, "Ready download not found"),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

// ── Handler: /cache (proxy to kl::server logic) ──────────────────────────────

async fn handle_cache(
    State(state): State<AppState>,
    Json(req): Json<kr::cache::CacheRequest>,
) -> Response {
    let result = parse_cache_request(&req);
    match result {
        Ok(movie) => {
            let num = movie.num.clone().unwrap_or_else(|| req.num.clone());
            let title = movie.title.clone();
            {
                let mut cache = state.cache.lock().unwrap();
                cache.insert_and_flush(&req.num, movie);
            }
            info!(num = %num, title = %title, "Cache entry stored");

            #[derive(Serialize)]
            struct CacheOk {
                ok: bool,
                num: String,
                title: String,
            }
            (
                StatusCode::OK,
                Json(CacheOk {
                    ok: true,
                    num,
                    title,
                }),
            )
                .into_response()
        }
        Err(e) => {
            error!(num = %req.num, error = %e, "Cache parse error");
            err_json(StatusCode::UNPROCESSABLE_ENTITY, e.to_string())
        }
    }
}

fn parse_cache_request(req: &kr::cache::CacheRequest) -> Result<kr::Movie> {
    match req.scraper_type.to_lowercase().as_str() {
        "javdb" => {
            let scraper = kl::javdb::JavdbScraper::new()?;
            scraper.parse_html(&req.num, &req.html)
        }
        "fc2" => {
            let scraper = kl::fc2::Fc2Scraper::new()?;
            let parser = scraper.parse_html(&req.num);
            parser(&req.html)
        }
        other => anyhow::bail!("Unknown scraper type: {}", other),
    }
}

// ── CLI ──────────────────────────────────────────────────────────────────────

#[derive(clap::Parser)]
#[command(
    author,
    version,
    about = "KS – central sync server for kk/kl JSON databases"
)]
struct Cli {
    /// Port to listen on
    #[arg(short, long, default_value = "7070")]
    port: u16,

    /// Directory to store database JSON files (defaults to kk config dir)
    #[arg(short, long)]
    data_dir: Option<PathBuf>,
}

// ── Entry-point ──────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    use clap::Parser;
    let cli = Cli::parse();

    let data_dir = cli
        .data_dir
        .unwrap_or_else(|| dirs::DIR.config_local_dir().to_path_buf());

    std::fs::create_dir_all(&data_dir).ok();

    // Persist pending list in the same directory as config.toml
    let queue_path = dirs::DIR.config_local_dir().join("ks_webdav_pending.json");
    let cache_dir =
        dirs::ks_download_cache_dir().unwrap_or_else(|| data_dir.join("ks_webdav_cache"));
    let download_dir = dirs::ks_download_target_dir().unwrap_or_else(|| cache_dir.clone());
    let source_prefixes = dirs::ks_webdav_source_prefixes();
    let max_total_bytes = dirs::ks_download_max_total_bytes().unwrap_or(200 * 1024 * 1024 * 1024); // 200 GiB

    let downloads = DownloadManager::load(
        queue_path.clone(),
        cache_dir.clone(),
        download_dir.clone(),
        source_prefixes.clone(),
        max_total_bytes,
    )
    .await
    .with_context(|| "Failed to initialize download manager")?;

    info!(
        queue_path = %queue_path.display(),
        cache_dir = %cache_dir.display(),
        download_dir = %download_dir.display(),
        source_prefixes = ?source_prefixes,
        max_total_bytes,
        "Initialized WebDAV background download queue"
    );

    tokio::spawn(download_worker_loop(downloads.clone()));

    let state = AppState {
        data_dir,
        cache: Arc::new(Mutex::new(kr::cache::KkCache::load())),
        downloads,
    };

    const MAX_BODY: usize = 20 * 1024 * 1024; // 20 MB

    let app = Router::new()
        .route("/db/kr", get(get_kr).put(put_kr))
        .route("/db/kwa", get(get_kwa).put(put_kwa))
        .route("/db/kk-cache", get(get_kk_cache).put(put_kk_cache))
        .route("/cache", post(handle_cache))
        .route(
            "/downloads/webdav",
            get(get_download_webdav).post(post_download_webdav),
        )
        .route("/downloads/webdav/ready", get(get_download_webdav_ready))
        .route(
            "/downloads/webdav/ready/{id}/file",
            get(get_download_webdav_ready_file),
        )
        .route(
            "/downloads/webdav/ready/{id}",
            delete(delete_download_webdav_ready),
        )
        .with_state(state)
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .layer(CorsLayer::permissive());

    let addr = SocketAddr::from(([0, 0, 0, 0], cli.port));
    info!("ks server listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_status_accepts_running_alias() {
        let status: PendingStatus = serde_json::from_str("\"running\"").unwrap();
        assert_eq!(status, PendingStatus::Downloading);
    }

    #[test]
    fn build_webdav_candidate_paths_replaces_only_first_directory() {
        let candidates = build_webdav_candidate_paths(
            "/legacy/path/abc.mp4",
            &["/mnt/media".to_string(), "incoming".to_string()],
        );

        assert_eq!(
            candidates,
            vec![
                "/mnt/media/path/abc.mp4".to_string(),
                "/incoming/path/abc.mp4".to_string(),
                "/legacy/path/abc.mp4".to_string()
            ]
        );
    }

    #[test]
    fn build_webdav_candidate_paths_deduplicates_original_path() {
        let candidates =
            build_webdav_candidate_paths("/incoming/abc.mp4", &["incoming".to_string()]);

        assert_eq!(candidates, vec!["/incoming/abc.mp4".to_string()]);
    }

    #[test]
    fn build_webdav_candidate_paths_also_tries_percent_decoded_variants() {
        let candidates =
            build_webdav_candidate_paths("/legacy/%5Babc%5D.mp4", &["/mnt/media".to_string()]);

        assert_eq!(
            candidates,
            vec![
                "/mnt/media/%5Babc%5D.mp4".to_string(),
                "/mnt/media/[abc].mp4".to_string(),
                "/legacy/%5Babc%5D.mp4".to_string(),
                "/legacy/[abc].mp4".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn load_treats_running_as_unfinished() {
        let uniq = format!("ks-queue-test-{}-{}", std::process::id(), now_ts());
        let base = std::env::temp_dir().join(uniq);
        let queue_path = base.join("ks_webdav_pending.json");
        let cache_dir = base.join("cache");
        let download_dir = base.join("downloads");
        tokio::fs::create_dir_all(&base).await.unwrap();

        let snapshot = r#"{
  "pending": [
    {
      "id": "1",
      "url_path": "/a.mp4",
      "file_name": "a.mp4",
      "file_size": 1,
      "source_base_url": "https://example.com",
      "source_user": null,
      "source_pass": null,
      "status": "running",
      "enqueued_at": 1
    },
    {
      "id": "2",
      "url_path": "/b.mp4",
      "file_name": "b.mp4",
      "file_size": 2,
      "source_base_url": "https://example.com",
      "source_user": null,
      "source_pass": null,
      "status": "downloading",
      "enqueued_at": 2
    }
  ],
  "ready": []
}"#;
        tokio::fs::write(&queue_path, snapshot).await.unwrap();

        let manager = DownloadManager::load(
            queue_path.clone(),
            cache_dir,
            download_dir,
            Vec::new(),
            1024,
        )
        .await
        .unwrap();

        let state = manager.state.lock().await;
        assert_eq!(state.pending.len(), 2);
        assert!(
            state
                .pending
                .iter()
                .all(|item| item.status == PendingStatus::Queued)
        );
        drop(state);

        let _ = tokio::fs::remove_dir_all(base).await;
    }
}
