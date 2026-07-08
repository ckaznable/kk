//! Minimal PikPak drive API client, ported from rclone's `backend/pikpak`.
//!
//! Instead of fetching file content through PikPak's WebDAV gateway (which
//! rejects or zero-lengths some files), this client signs in with the account
//! credentials, resolves a path to a file id via the drive API, and downloads
//! from the signed CDN link returned by `GET /drive/v1/files/{id}` — the same
//! flow rclone uses. Media links are preferred over the original
//! `application/octet-stream` link, mirroring rclone's default behaviour.

use anyhow::{Context, Result, anyhow, bail};
use base64::prelude::*;
use futures_util::StreamExt;
use md5::{Digest, Md5};
use reqwest::header::{HeaderMap, HeaderValue, RANGE};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Deserializer};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex as AsyncMutex;
use url::Url;

const CLIENT_ID: &str = "YUMx5nI8ZU8Ap8pm";
const CLIENT_VERSION: &str = "2.0.0";
const PACKAGE_NAME: &str = "mypikpak.com";
const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:129.0) Gecko/20100101 Firefox/129.0";
const API_ROOT: &str = "https://api-drive.mypikpak.com";
const USER_ROOT: &str = "https://user.mypikpak.com";
const SIGNIN_ACTION: &str = "POST:/v1/auth/signin";

const KIND_FILE: &str = "drive#file";
const KIND_FOLDER: &str = "drive#folder";
const PHASE_COMPLETE: &str = "PHASE_TYPE_COMPLETE";
const LIST_LIMIT: usize = 500;

const API_TIMEOUT: Duration = Duration::from_secs(60);
const API_TRANSIENT_RETRIES: u32 = 3;
/// rclone waits 5s between retries when the API reports a file without a
/// usable download link yet.
const LINK_RETRY_ATTEMPTS: u32 = 6;
const LINK_RETRY_DELAY: Duration = Duration::from_secs(5);
/// Counterpart of rclone's --low-level-retries for a single download stream.
const DOWNLOAD_STREAM_RETRIES: u32 = 10;
/// Treat links as expired slightly before their `expire` timestamp.
const LINK_EXPIRY_SLACK_SECS: i64 = 10;
/// Refuse to renew the token freshly before it actually expires.
const TOKEN_EXPIRY_SLACK: Duration = Duration::from_secs(60);
/// After a rejected signin, skip API attempts for a while so a wrong password
/// does not hammer the auth endpoint on every queued download.
const SIGNIN_BACKOFF: Duration = Duration::from_secs(300);

/// Same salt chain rclone uses to compute `captcha_sign` (`helper.go`).
const MD5_SALTS: [&str; 15] = [
    "C9qPpZLN8ucRTaTiUMWYS9cQvWOE",
    "+r6CQVxjzJV6LCV",
    "F",
    "pFJRC",
    "9WXYIDGrwTCz2OiVlgZa90qpECPD6olt",
    "/750aCr4lm/Sly/c",
    "RB+DT/gZCrbV",
    "",
    "CyLsf7hdkIRxRm215hl",
    "7xHvLi2tOYP0Y92b",
    "ZGTXXxu8E/MIWaEDB+Sm/",
    "1UI3",
    "E7fP5Pfijd+7K+t6Tg/NhuLq0eEUVChpJSkrKxpO",
    "ihtqpG6FMt65+Xk+tWUH2",
    "NhXXU9rg4XXdzo7u5o",
];

// ── API response types ───────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
struct ApiErrorBody {
    #[serde(default)]
    error: String,
    #[serde(default)]
    error_code: i64,
    #[serde(default)]
    error_description: String,
}

#[derive(Debug, Default, Deserialize)]
struct TokenResponse {
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    #[serde(default)]
    expires_in: i64,
    #[serde(default)]
    sub: String,
}

#[derive(Debug, Default, Deserialize)]
struct CaptchaResponse {
    #[serde(default)]
    captcha_token: String,
    #[serde(default)]
    expires_in: i64,
    #[serde(default)]
    url: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LinkInfo {
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct FileLinks {
    #[serde(rename = "application/octet-stream", default)]
    pub octet_stream: Option<LinkInfo>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct MediaInfo {
    #[serde(default)]
    pub media_name: String,
    #[serde(default)]
    pub link: Option<LinkInfo>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct FileInfo {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, deserialize_with = "de_size")]
    pub size: Option<u64>,
    #[serde(default)]
    pub trashed: bool,
    #[serde(default)]
    pub links: Option<FileLinks>,
    #[serde(default)]
    pub medias: Vec<MediaInfo>,
}

impl FileInfo {
    pub fn is_folder(&self) -> bool {
        self.kind == KIND_FOLDER
    }

    fn octet_link(&self) -> Option<&LinkInfo> {
        self.links
            .as_ref()?
            .octet_stream
            .as_ref()
            .filter(|l| !l.url.is_empty())
    }
}

#[derive(Debug, Default, Deserialize)]
struct FileListResponse {
    #[serde(default)]
    files: Vec<FileInfo>,
    #[serde(default)]
    next_page_token: String,
}

/// The API serializes `size` as a JSON string; accept both string and number.
fn de_size<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(serde_json::Value::String(s)) => s.trim().parse().ok(),
        Some(serde_json::Value::Number(n)) => n.as_u64(),
        _ => None,
    })
}

// ── Hashing / signing helpers (ported from rclone helper.go) ─────────────────

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0F) as usize] as char);
    }
    out
}

fn md5_hex(input: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(input.as_bytes());
    hex_lower(&hasher.finalize())
}

/// Deterministic device id derived from the username. rclone generates a
/// random UUIDv4-shaped hex string and persists it in rclone.conf; deriving it
/// from the username gives the same stability without extra state.
pub fn device_id_for(username: &str) -> String {
    let mut hex = md5_hex(username).into_bytes();
    hex[12] = b'4';
    let nibble = match hex[16] {
        b @ b'0'..=b'9' => b - b'0',
        b @ b'a'..=b'f' => b - b'a' + 10,
        _ => 0,
    };
    hex[16] = b"0123456789abcdef"[((nibble & 3) | 8) as usize];
    String::from_utf8(hex).expect("md5 hex is always ASCII")
}

fn calc_captcha_sign(device_id: &str, timestamp: &str) -> String {
    let mut s = format!("{CLIENT_ID}{CLIENT_VERSION}{PACKAGE_NAME}{device_id}{timestamp}");
    for salt in MD5_SALTS {
        s = md5_hex(&format!("{s}{salt}"));
    }
    format!("1.{s}")
}

fn unix_millis_now() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .to_string()
}

/// Extract the `sub` (user id) claim from a JWT access token.
fn jwt_sub(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?.trim_end_matches('=');
    let decoded = BASE64_URL_SAFE_NO_PAD
        .decode(payload)
        .ok()
        .or_else(|| BASE64_STANDARD_NO_PAD.decode(payload).ok())?;

    #[derive(Deserialize)]
    struct Claims {
        #[serde(default)]
        sub: String,
    }

    serde_json::from_slice::<Claims>(&decoded)
        .ok()
        .map(|c| c.sub)
        .filter(|s| !s.is_empty())
}

// ── Link helpers (ported from rclone api/types.go + pikpak.go) ───────────────

/// `fid` query parameter of a download link, used to match media links to the
/// original file link (rclone `parseFileID`).
fn parse_file_id(link_url: &str) -> Option<String> {
    let url = Url::parse(link_url).ok()?;
    url.query_pairs()
        .find(|(k, _)| k == "fid")
        .map(|(_, v)| v.into_owned())
        .filter(|v| !v.is_empty())
}

/// Signed links carry an `expire` unix-seconds query parameter. Missing or
/// unparseable expiry means "assume valid" — download errors trigger a link
/// refresh anyway.
fn link_expired(link_url: &str) -> bool {
    let Ok(url) = Url::parse(link_url) else {
        return false;
    };
    for (key, value) in url.query_pairs() {
        if key == "expire"
            && let Ok(expire) = value.parse::<i64>()
        {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            return now + LINK_EXPIRY_SLACK_SECS >= expire;
        }
    }
    false
}

fn link_usable(link: &LinkInfo) -> bool {
    !link.url.is_empty() && !link_expired(&link.url)
}

/// Pick the download URL for a file, preferring the media link that matches
/// the original link's `fid` (rclone `setMetaData` with `no_media_link=false`).
/// Unlike rclone we additionally fall back to any valid media link when the
/// original link is absent — that is exactly the case for some blocked files.
fn pick_download_url(info: &FileInfo) -> Option<String> {
    if let Some(octet) = info.octet_link().filter(|l| link_usable(l)) {
        if let Some(fid) = parse_file_id(&octet.url) {
            for media in &info.medias {
                if let Some(link) = &media.link
                    && link_usable(link)
                    && parse_file_id(&link.url).as_deref() == Some(fid.as_str())
                {
                    log::debug!("pikpak: using media link {:?}", media.media_name);
                    return Some(link.url.clone());
                }
            }
        }
        return Some(octet.url.clone());
    }

    for media in &info.medias {
        if let Some(link) = &media.link
            && link_usable(link)
        {
            log::debug!(
                "pikpak: original link missing, using media link {:?}",
                media.media_name
            );
            return Some(link.url.clone());
        }
    }

    None
}

// ── Client ───────────────────────────────────────────────────────────────────

#[derive(Default)]
struct AuthState {
    access_token: String,
    refresh_token: String,
    token_expires_at: Option<SystemTime>,
    user_id: String,
    captcha_token: String,
    captcha_expires_at: Option<SystemTime>,
    signin_backoff_until: Option<Instant>,
}

impl AuthState {
    fn valid_captcha(&self) -> Option<String> {
        if self.captcha_token.is_empty() {
            return None;
        }
        if let Some(expires_at) = self.captcha_expires_at
            && SystemTime::now() + Duration::from_secs(10) >= expires_at
        {
            return None;
        }
        Some(self.captcha_token.clone())
    }
}

pub struct PikPakClient {
    http: Client,
    username: String,
    password: String,
    device_id: String,
    auth: AsyncMutex<AuthState>,
}

impl PikPakClient {
    pub fn new(username: &str, password: &str) -> Result<Self> {
        let username = username.trim();
        if username.is_empty() || password.is_empty() {
            bail!("PikPak username and password must not be empty");
        }

        let device_id = device_id_for(username);
        let mut headers = HeaderMap::new();
        headers.insert("Referer", HeaderValue::from_static("https://mypikpak.com/"));
        headers.insert("x-client-id", HeaderValue::from_static(CLIENT_ID));
        headers.insert("x-client-version", HeaderValue::from_static(CLIENT_VERSION));
        headers.insert("x-device-id", HeaderValue::from_str(&device_id)?);

        let http = Client::builder()
            .user_agent(USER_AGENT)
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::limited(10))
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(120))
            .build()?;

        Ok(Self {
            http,
            username: username.to_string(),
            password: password.to_string(),
            device_id,
            auth: AsyncMutex::new(AuthState::default()),
        })
    }

    // ── auth ─────────────────────────────────────────────────────────────

    async fn ensure_access_token(&self) -> Result<String> {
        let mut auth = self.auth.lock().await;

        let expired = auth.access_token.is_empty()
            || auth
                .token_expires_at
                .map(|t| SystemTime::now() + TOKEN_EXPIRY_SLACK >= t)
                .unwrap_or(true);
        if !expired {
            return Ok(auth.access_token.clone());
        }

        if let Some(until) = auth.signin_backoff_until {
            if Instant::now() < until {
                bail!("PikPak signin recently rejected; backing off before retrying");
            }
            auth.signin_backoff_until = None;
        }

        let mut refreshed = false;
        if !auth.refresh_token.is_empty() {
            match self.refresh_access_token(&mut auth).await {
                Ok(()) => refreshed = true,
                Err(e) => log::warn!("PikPak token refresh failed, falling back to signin: {e}"),
            }
        }
        if !refreshed {
            self.signin(&mut auth).await?;
        }

        Ok(auth.access_token.clone())
    }

    async fn invalidate_access_token(&self) {
        let mut auth = self.auth.lock().await;
        auth.access_token.clear();
        auth.token_expires_at = None;
    }

    async fn invalidate_captcha_token(&self) {
        let mut auth = self.auth.lock().await;
        auth.captcha_token.clear();
        auth.captcha_expires_at = None;
    }

    fn apply_token(&self, auth: &mut AuthState, token: TokenResponse) -> Result<()> {
        if token.access_token.is_empty() {
            bail!("PikPak auth response is missing access_token");
        }
        auth.user_id = if !token.sub.is_empty() {
            token.sub.clone()
        } else {
            jwt_sub(&token.access_token).unwrap_or_default()
        };
        auth.access_token = token.access_token;
        if !token.refresh_token.is_empty() {
            auth.refresh_token = token.refresh_token;
        }
        let ttl = if token.expires_in > 0 {
            token.expires_in as u64
        } else {
            3600
        };
        auth.token_expires_at = Some(SystemTime::now() + Duration::from_secs(ttl));
        Ok(())
    }

    async fn signin(&self, auth: &mut AuthState) -> Result<()> {
        log::info!("Signing in to PikPak API as {}", self.username);

        let mut result = Err(anyhow!("PikPak signin not attempted"));
        for attempt in 0..2 {
            let captcha = self.request_captcha_token(auth, SIGNIN_ACTION).await?;
            let resp = self
                .http
                .post(format!("{USER_ROOT}/v1/auth/signin"))
                .timeout(API_TIMEOUT)
                .header("x-captcha-token", captcha)
                .json(&serde_json::json!({
                    "username": self.username,
                    "password": self.password,
                    "client_id": CLIENT_ID,
                }))
                .send()
                .await?;

            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if status.is_success() {
                let token: TokenResponse = serde_json::from_str(&text)
                    .context("failed to parse PikPak signin response")?;
                self.apply_token(auth, token)?;
                auth.signin_backoff_until = None;
                log::info!("PikPak signin succeeded (user_id={})", auth.user_id);
                return Ok(());
            }

            let api_err: ApiErrorBody = serde_json::from_str(&text).unwrap_or_default();
            result = Err(anyhow!(
                "PikPak signin failed: HTTP {} error={:?} code={} desc={:?}",
                status,
                api_err.error,
                api_err.error_code,
                api_err.error_description
            ));

            // rclone retries the signin once with a fresh captcha token.
            if api_err.error == "captcha_invalid" && attempt == 0 {
                log::warn!("PikPak signin captcha invalid; retrying with a fresh captcha token");
                auth.captcha_token.clear();
                auth.captcha_expires_at = None;
                continue;
            }
            break;
        }

        auth.signin_backoff_until = Some(Instant::now() + SIGNIN_BACKOFF);
        result
    }

    async fn refresh_access_token(&self, auth: &mut AuthState) -> Result<()> {
        log::info!("Refreshing PikPak API access token");
        let resp = self
            .http
            .post(format!("{USER_ROOT}/v1/auth/token"))
            .timeout(API_TIMEOUT)
            .form(&[
                ("client_id", CLIENT_ID),
                ("grant_type", "refresh_token"),
                ("refresh_token", auth.refresh_token.as_str()),
            ])
            .send()
            .await?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            // The refresh token may have been rotated or revoked; force signin.
            auth.refresh_token.clear();
            let api_err: ApiErrorBody = serde_json::from_str(&text).unwrap_or_default();
            bail!(
                "PikPak token refresh failed: HTTP {} error={:?} code={} desc={:?}",
                status,
                api_err.error,
                api_err.error_code,
                api_err.error_description
            );
        }

        let token: TokenResponse =
            serde_json::from_str(&text).context("failed to parse PikPak token response")?;
        self.apply_token(auth, token)
    }

    /// Return a captcha token valid for API calls, requesting a new one when
    /// the cached token is missing or expired (rclone `CaptchaTokenSource`).
    async fn request_captcha_token(&self, auth: &mut AuthState, action: &str) -> Result<String> {
        if let Some(token) = auth.valid_captcha() {
            return Ok(token);
        }

        let meta = if action == SIGNIN_ACTION {
            serde_json::json!({ "username": self.username })
        } else {
            let timestamp = unix_millis_now();
            serde_json::json!({
                "captcha_sign": calc_captcha_sign(&self.device_id, &timestamp),
                "client_version": CLIENT_VERSION,
                "package_name": PACKAGE_NAME,
                "timestamp": timestamp,
                "user_id": auth.user_id,
            })
        };

        let resp = self
            .http
            .post(format!("{USER_ROOT}/v1/shield/captcha/init"))
            .timeout(API_TIMEOUT)
            .json(&serde_json::json!({
                "action": action,
                "captcha_token": auth.captcha_token,
                "client_id": CLIENT_ID,
                "device_id": self.device_id,
                "meta": meta,
            }))
            .send()
            .await?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            let api_err: ApiErrorBody = serde_json::from_str(&text).unwrap_or_default();
            bail!(
                "PikPak captcha init failed: HTTP {} error={:?} code={} desc={:?}",
                status,
                api_err.error,
                api_err.error_code,
                api_err.error_description
            );
        }

        let captcha: CaptchaResponse =
            serde_json::from_str(&text).context("failed to parse PikPak captcha response")?;
        if captcha.captcha_token.is_empty() {
            if !captcha.url.is_empty() {
                bail!(
                    "PikPak requires solving a captcha in a browser: {}",
                    captcha.url
                );
            }
            bail!("PikPak captcha init returned an empty token");
        }

        auth.captcha_token = captcha.captcha_token.clone();
        auth.captcha_expires_at = (captcha.expires_in > 0)
            .then(|| SystemTime::now() + Duration::from_secs(captcha.expires_in.max(10) as u64));
        Ok(captcha.captcha_token)
    }

    async fn ensure_captcha_token(&self, action: &str) -> Result<String> {
        let mut auth = self.auth.lock().await;
        self.request_captcha_token(&mut auth, action).await
    }

    // ── API calls ────────────────────────────────────────────────────────

    async fn api_get(&self, path: &str, query: &[(String, String)]) -> Result<serde_json::Value> {
        let action = format!("GET:{path}");
        let mut reauthed = false;
        let mut captcha_refreshed = false;
        let mut transient = 0u32;

        loop {
            let token = self.ensure_access_token().await?;
            let captcha = self.ensure_captcha_token(&action).await?;

            let resp = match self
                .http
                .get(format!("{API_ROOT}{path}"))
                .timeout(API_TIMEOUT)
                .query(query)
                .bearer_auth(&token)
                .header("x-captcha-token", captcha)
                .send()
                .await
            {
                Ok(resp) => resp,
                Err(e) => {
                    if transient < API_TRANSIENT_RETRIES {
                        transient += 1;
                        tokio::time::sleep(Duration::from_millis(500 * u64::from(transient))).await;
                        continue;
                    }
                    return Err(anyhow!("PikPak API GET {path} failed: {e}"));
                }
            };

            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if status.is_success() {
                return serde_json::from_str(&text)
                    .with_context(|| format!("invalid JSON from PikPak API GET {path}"));
            }

            let api_err: ApiErrorBody = serde_json::from_str(&text).unwrap_or_default();

            // "unauthenticated" (16): access token expired or invalidated.
            if (status == StatusCode::UNAUTHORIZED || api_err.error_code == 16) && !reauthed {
                reauthed = true;
                log::warn!("PikPak API GET {path} unauthenticated; re-authorizing");
                self.invalidate_access_token().await;
                continue;
            }

            if api_err.error == "captcha_invalid" && !captcha_refreshed {
                captcha_refreshed = true;
                log::warn!("PikPak API GET {path} rejected captcha; refreshing captcha token");
                self.invalidate_captcha_token().await;
                continue;
            }

            let retryable = matches!(status.as_u16(), 429 | 500 | 502 | 503 | 504 | 509);
            if retryable && transient < API_TRANSIENT_RETRIES {
                transient += 1;
                tokio::time::sleep(Duration::from_millis(500 * u64::from(transient))).await;
                continue;
            }

            return Err(anyhow!(
                "PikPak API GET {path} failed: HTTP {} error={:?} code={} desc={:?}",
                status,
                api_err.error,
                api_err.error_code,
                api_err.error_description
            ));
        }
    }

    /// Find a direct child of `parent_id` (empty = drive root) by name.
    async fn find_child(&self, parent_id: &str, name: &str) -> Result<Option<FileInfo>> {
        let filters = serde_json::json!({
            "phase": {"eq": PHASE_COMPLETE},
            "trashed": {"eq": false},
        })
        .to_string();

        let mut page_token = String::new();
        let mut case_insensitive_match: Option<FileInfo> = None;
        loop {
            let mut query: Vec<(String, String)> = vec![
                ("thumbnail_size".into(), "SIZE_MEDIUM".into()),
                ("limit".into(), LIST_LIMIT.to_string()),
                ("with_audit".into(), "true".into()),
                ("filters".into(), filters.clone()),
            ];
            if !parent_id.is_empty() {
                query.push(("parent_id".into(), parent_id.to_string()));
            }
            if !page_token.is_empty() {
                query.push(("page_token".into(), page_token.clone()));
            }

            let value = self.api_get("/drive/v1/files", &query).await?;
            let list: FileListResponse =
                serde_json::from_value(value).context("failed to parse PikPak file list")?;

            for file in list.files {
                if file.name == name {
                    return Ok(Some(file));
                }
                if case_insensitive_match.is_none() && file.name.eq_ignore_ascii_case(name) {
                    case_insensitive_match = Some(file);
                }
            }

            if list.next_page_token.is_empty() {
                break;
            }
            page_token = list.next_page_token;
        }

        Ok(case_insensitive_match)
    }

    /// Resolve a slash-separated drive path to its file entry by walking the
    /// folder tree from the drive root.
    pub async fn resolve_path(&self, path: &str) -> Result<Option<FileInfo>> {
        let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        if segments.is_empty() {
            return Ok(None);
        }

        let mut parent_id = String::new();
        for (idx, segment) in segments.iter().enumerate() {
            let Some(found) = self.find_child(&parent_id, segment).await? else {
                return Ok(None);
            };
            if idx + 1 == segments.len() {
                return Ok(Some(found));
            }
            if !found.is_folder() {
                return Ok(None);
            }
            parent_id = found.id.clone();
        }
        Ok(None)
    }

    /// Fetch rich file info (incl. download links), retrying while the API
    /// has not produced a usable link yet (rclone `getFile`).
    pub async fn get_file(&self, id: &str) -> Result<FileInfo> {
        let path = format!("/drive/v1/files/{id}");
        for attempt in 1..=LINK_RETRY_ATTEMPTS {
            let value = self.api_get(&path, &[]).await?;
            let info: FileInfo =
                serde_json::from_value(value).context("failed to parse PikPak file info")?;
            if info.is_folder() || pick_download_url(&info).is_some() {
                return Ok(info);
            }
            log::warn!(
                "PikPak API returned no usable download link for {id} (attempt {attempt}/{LINK_RETRY_ATTEMPTS})"
            );
            if attempt < LINK_RETRY_ATTEMPTS {
                tokio::time::sleep(LINK_RETRY_DELAY).await;
            }
        }
        bail!(
            "PikPak API returned no usable download link for {id} after {LINK_RETRY_ATTEMPTS} attempts — \
             the file may be blocked from downloading"
        )
    }

    // ── download ─────────────────────────────────────────────────────────

    /// Resolve `remote_path` on the drive and download it to `dest`.
    /// Progress is reported as `(written_bytes, total_bytes)`.
    pub async fn download_with_progress<F>(
        &self,
        remote_path: &str,
        dest: &Path,
        on_progress: F,
    ) -> Result<u64>
    where
        F: FnMut(u64, Option<u64>),
    {
        let info = self
            .resolve_path(remote_path)
            .await?
            .ok_or_else(|| anyhow!("path not found on PikPak drive: {remote_path}"))?;
        if info.is_folder() {
            bail!("PikPak path is a folder, not a file: {remote_path}");
        }
        if info.kind != KIND_FILE {
            bail!(
                "PikPak path {remote_path} is not a downloadable file (kind={})",
                info.kind
            );
        }
        self.download_file_by_id(&info.id, dest, on_progress).await
    }

    /// Download a file by id from its signed CDN link, resuming with `Range`
    /// requests on stream errors and refreshing the link when it expires.
    pub async fn download_file_by_id<F>(
        &self,
        id: &str,
        dest: &Path,
        mut on_progress: F,
    ) -> Result<u64>
    where
        F: FnMut(u64, Option<u64>),
    {
        use tokio::io::{AsyncWriteExt, BufWriter};

        let mut info = self.get_file(id).await?;
        let mut link_url = pick_download_url(&info).ok_or_else(|| {
            anyhow!(
                "no download link for PikPak file {:?} — check sharing permission / audit status",
                info.name
            )
        })?;

        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temp_dest = crate::temp_download_path(dest);
        crate::cleanup_partial_download(&temp_dest).await;

        async fn fail<T>(temp_dest: &Path, err: anyhow::Error) -> Result<T> {
            crate::cleanup_partial_download(temp_dest).await;
            Err(err)
        }

        let file = tokio::fs::File::create(&temp_dest).await?;
        let mut writer = BufWriter::with_capacity(crate::DOWNLOAD_WRITE_BUFFER_SIZE, file);
        let mut written: u64 = 0;
        let mut total: Option<u64> = info.size.filter(|s| *s > 0);
        let mut next_progress_pct: u64 = 5;
        let mut retries: u32 = 0;

        loop {
            if link_expired(&link_url) {
                log::info!("PikPak download link for {id} expired; requesting a fresh one");
                info = self.get_file(id).await?;
                link_url = pick_download_url(&info)
                    .ok_or_else(|| anyhow!("no fresh download link for PikPak file {id}"))?;
            }

            let mut request = self.http.get(&link_url);
            if written > 0 {
                request = request.header(RANGE, format!("bytes={written}-"));
            }

            let resp = match request.send().await {
                Ok(resp) => resp,
                Err(e) => {
                    if retries >= DOWNLOAD_STREAM_RETRIES {
                        return fail(
                            &temp_dest,
                            anyhow!(
                                "PikPak download request failed for {id} (written={written}): {e}"
                            ),
                        )
                        .await;
                    }
                    retries += 1;
                    tokio::time::sleep(Duration::from_millis((500 * u64::from(retries)).min(5000)))
                        .await;
                    continue;
                }
            };

            let status = resp.status();

            // Signed link rejected — most likely expired or rotated server-side.
            if matches!(
                status,
                StatusCode::FORBIDDEN | StatusCode::GONE | StatusCode::NOT_FOUND
            ) {
                if retries >= DOWNLOAD_STREAM_RETRIES {
                    return fail(
                        &temp_dest,
                        anyhow!("PikPak CDN rejected the download link for {id}: HTTP {status}"),
                    )
                    .await;
                }
                retries += 1;
                log::warn!(
                    "PikPak CDN returned HTTP {status} for {id}; refreshing link (retry {retries})"
                );
                info = self.get_file(id).await?;
                link_url = pick_download_url(&info)
                    .ok_or_else(|| anyhow!("no fresh download link for PikPak file {id}"))?;
                continue;
            }

            if !status.is_success() {
                let retryable = matches!(status.as_u16(), 429 | 500 | 502 | 503 | 504 | 509);
                if retryable && retries < DOWNLOAD_STREAM_RETRIES {
                    retries += 1;
                    tokio::time::sleep(Duration::from_millis((500 * u64::from(retries)).min(5000)))
                        .await;
                    continue;
                }
                return fail(
                    &temp_dest,
                    anyhow!(
                        "PikPak download failed for {id}: HTTP {status} (written={written}, total={total:?})"
                    ),
                )
                .await;
            }

            if written > 0 && status != StatusCode::PARTIAL_CONTENT {
                // Server ignored our Range request — restart from scratch.
                log::warn!("PikPak CDN ignored Range request for {id}; restarting download from 0");
                let file = match tokio::fs::File::create(&temp_dest).await {
                    Ok(f) => f,
                    Err(e) => return fail(&temp_dest, e.into()).await,
                };
                writer = BufWriter::with_capacity(crate::DOWNLOAD_WRITE_BUFFER_SIZE, file);
                written = 0;
                next_progress_pct = 5;
            }

            if total.is_none() {
                total = resp
                    .content_length()
                    .map(|len| written + len)
                    .filter(|t| *t > 0);
            }

            let mut stream = resp.bytes_stream();
            let mut stream_failed = false;
            while let Some(chunk) = stream.next().await {
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    Err(e) => {
                        log::warn!("PikPak download stream error for {id} at {written} bytes: {e}");
                        stream_failed = true;
                        break;
                    }
                };
                written += chunk.len() as u64;
                if let Err(e) = writer.write_all(&chunk).await {
                    return fail(
                        &temp_dest,
                        anyhow!(
                            "write error while downloading PikPak file {id} to {} (written={written}): {e}",
                            temp_dest.display()
                        ),
                    )
                    .await;
                }

                if let Some(total) = total {
                    while next_progress_pct <= 100
                        && written.saturating_mul(100) >= total.saturating_mul(next_progress_pct)
                    {
                        on_progress(written, Some(total));
                        next_progress_pct += 5;
                    }
                }
            }

            if !stream_failed {
                match total {
                    Some(t) if written > t => {
                        return fail(
                            &temp_dest,
                            anyhow!(
                                "PikPak download for {id} produced more bytes than expected ({written} > {t})"
                            ),
                        )
                        .await;
                    }
                    // Premature EOF without a transport error — resume below.
                    Some(t) if written < t => {}
                    _ => break,
                }
            }

            if retries >= DOWNLOAD_STREAM_RETRIES {
                return fail(
                    &temp_dest,
                    anyhow!(
                        "PikPak download for {id} failed after {retries} resume attempts (written={written}, total={total:?})"
                    ),
                )
                .await;
            }
            retries += 1;
            log::warn!(
                "Resuming PikPak download for {id} from byte {written} (retry {retries}/{DOWNLOAD_STREAM_RETRIES})"
            );
            tokio::time::sleep(Duration::from_millis((500 * u64::from(retries)).min(5000))).await;
        }

        if let Err(e) = writer.flush().await {
            return fail(
                &temp_dest,
                anyhow!(
                    "flush error while downloading PikPak file {id} to {} (written={written}): {e}",
                    temp_dest.display()
                ),
            )
            .await;
        }

        if written == 0 {
            return fail(
                &temp_dest,
                anyhow!("PikPak download for {id} produced 0 bytes"),
            )
            .await;
        }

        if let Some(t) = total
            && written != t
        {
            return fail(
                &temp_dest,
                anyhow!("PikPak download size mismatch for {id}: expected {t}, got {written}"),
            )
            .await;
        }

        let file = writer.into_inner();
        #[cfg(unix)]
        {
            file.sync_data().await.ok();
            crate::evict_file_from_page_cache(&file, &temp_dest);
        }
        drop(file);

        tokio::fs::rename(&temp_dest, dest).await?;

        if total.is_none() {
            on_progress(written, None);
        }

        Ok(written)
    }
}

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_id_is_stable_and_uuidv4_shaped() {
        let a = device_id_for("user@example.com");
        let b = device_id_for("user@example.com");
        assert_eq!(a, b);
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(a.as_bytes()[12], b'4');
        assert!(matches!(a.as_bytes()[16], b'8' | b'9' | b'a' | b'b'));
        assert_ne!(a, device_id_for("other@example.com"));
    }

    #[test]
    fn captcha_sign_is_deterministic_and_formatted() {
        let sign = calc_captcha_sign("0123456789ab4def89abcdef01234567", "1700000000000");
        assert_eq!(
            sign,
            calc_captcha_sign("0123456789ab4def89abcdef01234567", "1700000000000")
        );
        assert!(sign.starts_with("1."));
        assert_eq!(sign.len(), 2 + 32);
        assert!(sign[2..].bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn parse_file_id_extracts_fid_query() {
        assert_eq!(
            parse_file_id("https://dl.example.com/download?fid=abc123&expire=1"),
            Some("abc123".to_string())
        );
        assert_eq!(parse_file_id("https://dl.example.com/download?x=1"), None);
        assert_eq!(parse_file_id("not a url"), None);
    }

    #[test]
    fn link_expired_checks_expire_query_parameter() {
        assert!(link_expired(
            "https://dl.example.com/download?fid=a&expire=1000000000"
        ));
        assert!(!link_expired(
            "https://dl.example.com/download?fid=a&expire=4102444800"
        ));
        // No expire parameter → assume valid.
        assert!(!link_expired("https://dl.example.com/download?fid=a"));
    }

    #[test]
    fn pick_download_url_prefers_matching_media_link() {
        let info: FileInfo = serde_json::from_str(
            r#"{
                "id": "f1",
                "kind": "drive#file",
                "name": "a.mp4",
                "size": "1048576",
                "links": {"application/octet-stream": {"url": "https://dl.example.com/o?fid=f1&expire=4102444800"}},
                "medias": [
                    {"media_name": "other", "link": {"url": "https://dl.example.com/m?fid=zz&expire=4102444800"}},
                    {"media_name": "raw", "link": {"url": "https://dl.example.com/m?fid=f1&expire=4102444800"}}
                ]
            }"#,
        )
        .unwrap();
        assert_eq!(info.size, Some(1048576));
        assert_eq!(
            pick_download_url(&info).unwrap(),
            "https://dl.example.com/m?fid=f1&expire=4102444800"
        );
    }

    #[test]
    fn pick_download_url_falls_back_to_octet_stream_then_media() {
        let octet_only: FileInfo = serde_json::from_str(
            r#"{
                "id": "f1", "kind": "drive#file", "name": "a.mp4",
                "links": {"application/octet-stream": {"url": "https://dl.example.com/o?fid=f1"}},
                "medias": []
            }"#,
        )
        .unwrap();
        assert_eq!(
            pick_download_url(&octet_only).unwrap(),
            "https://dl.example.com/o?fid=f1"
        );

        let media_only: FileInfo = serde_json::from_str(
            r#"{
                "id": "f1", "kind": "drive#file", "name": "a.mp4",
                "medias": [{"media_name": "raw", "link": {"url": "https://dl.example.com/m?fid=f1"}}]
            }"#,
        )
        .unwrap();
        assert_eq!(
            pick_download_url(&media_only).unwrap(),
            "https://dl.example.com/m?fid=f1"
        );

        let none: FileInfo =
            serde_json::from_str(r#"{"id": "f1", "kind": "drive#file", "name": "a.mp4"}"#).unwrap();
        assert!(pick_download_url(&none).is_none());
    }

    #[test]
    fn file_size_accepts_string_and_number() {
        let s: FileInfo = serde_json::from_str(r#"{"size": "123"}"#).unwrap();
        assert_eq!(s.size, Some(123));
        let n: FileInfo = serde_json::from_str(r#"{"size": 456}"#).unwrap();
        assert_eq!(n.size, Some(456));
        let missing: FileInfo = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(missing.size, None);
    }
}
