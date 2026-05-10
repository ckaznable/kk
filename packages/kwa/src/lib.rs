use anyhow::{Result, anyhow};
use base64::prelude::*;
use futures_util::StreamExt;
use percent_encoding::percent_decode_str;
use reqwest::Client;
use reqwest::header::{AUTHORIZATION, CONNECTION, CONTENT_RANGE, HeaderMap, HeaderValue, RANGE};
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use tokio::process::Command;
use url::Url;

const DOWNLOAD_WRITE_BUFFER_SIZE: usize = 2 * 1024 * 1024;
const RANGE_FALLBACK_CHUNK_SIZE: u64 = 32 * 1024 * 1024;

#[cfg(unix)]
fn evict_file_from_page_cache(file: &tokio::fs::File, path: &Path) {
    let rc = unsafe { libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
    if rc != 0 {
        log::warn!(
            "posix_fadvise(DONTNEED) failed for {}: {}",
            path.display(),
            std::io::Error::from_raw_os_error(rc)
        );
    }
}

fn temp_download_path(dest: &Path) -> PathBuf {
    let file_name = dest
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".to_string());
    dest.with_file_name(format!("{file_name}.part"))
}

async fn cleanup_partial_download(path: &Path) {
    if let Err(e) = tokio::fs::remove_file(path).await
        && e.kind() != std::io::ErrorKind::NotFound
    {
        log::warn!(
            "failed to remove partial download {}: {}",
            path.display(),
            e
        );
    }
}

fn header_value(headers: &HeaderMap, name: &'static str) -> String {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("-")
        .to_string()
}

fn is_hex_digit(b: u8) -> bool {
    b.is_ascii_hexdigit()
}

fn has_url_encoded_bytes(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes
        .windows(3)
        .any(|window| window[0] == b'%' && is_hex_digit(window[1]) && is_hex_digit(window[2]))
}

fn normalize_standard_path(path: &str) -> String {
    let normalized = path.trim().replace('\\', "/");
    if !has_url_encoded_bytes(&normalized) {
        return normalized;
    }
    match percent_decode_str(&normalized).decode_utf8() {
        Ok(decoded) => decoded.into_owned(),
        Err(_) => normalized,
    }
}

pub fn encode_webdav_path_for_url(path: &str) -> String {
    let normalized = normalize_standard_path(path);
    let mut out = String::with_capacity(normalized.len() * 3);
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    for &b in normalized.as_bytes() {
        let is_unreserved =
            b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'.' | b'_' | b'~');
        if is_unreserved {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0F) as usize] as char);
        }
    }

    out
}

fn build_client(http1_only: bool) -> Result<Client> {
    let mut builder = Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .user_agent(format!("kk-webdav/{}", env!("CARGO_PKG_VERSION")));
    if http1_only {
        builder = builder.http1_only().pool_max_idle_per_host(0);
    }
    Ok(builder.build()?)
}

fn parse_content_range_total(headers: &HeaderMap) -> Option<u64> {
    let value = headers.get(CONTENT_RANGE)?.to_str().ok()?;
    let total = value.split('/').nth(1)?;
    if total == "*" {
        return None;
    }
    total.parse().ok()
}

fn parse_curl_write_out(stdout: &[u8]) -> (Option<u16>, Option<u64>, Option<String>) {
    let text = String::from_utf8_lossy(stdout);
    let mut http_code = None;
    let mut size_download = None;
    let mut url_effective = None;

    for line in text.lines() {
        if let Some(value) = line.strip_prefix("http_code=") {
            http_code = value.parse::<u16>().ok();
        } else if let Some(value) = line.strip_prefix("size_download=") {
            size_download = value.parse::<f64>().ok().map(|n| n.round() as u64);
        } else if let Some(value) = line.strip_prefix("url_effective=") {
            url_effective = Some(value.to_string());
        }
    }

    (http_code, size_download, url_effective)
}

#[derive(Debug, Clone)]
pub struct WebDavClient {
    client: Client,
    fallback_client: Client,
    base_url: Url,
    auth_header: Option<HeaderValue>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct WebDavResource {
    pub path: String,
    pub is_dir: bool,
    pub size: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct Prop {
    #[serde(rename = "resourcetype")]
    resource_type: Option<ResourceType>,
    #[serde(rename = "getcontentlength")]
    content_length: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ResourceType {
    collection: Option<()>,
}

#[derive(Debug, Deserialize)]
struct Propstat {
    prop: Prop,
}

#[derive(Debug, Deserialize)]
struct Response {
    href: String,
    propstat: Propstat,
}

#[derive(Debug, Deserialize)]
#[serde(rename = "multistatus")]
struct Multistatus {
    response: Vec<Response>,
}

impl WebDavClient {
    pub fn new(base_url: &str, auth: Option<(String, String)>) -> Result<Self> {
        let mut base = Url::parse(base_url)?;
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }

        let auth_header = auth.map(|(user, pass)| {
            let auth = format!("{}:{}", user, pass);
            let encoded = BASE64_STANDARD.encode(auth);
            HeaderValue::from_str(&format!("Basic {}", encoded)).unwrap()
        });

        let client = build_client(false)?;
        let fallback_client = build_client(true)?;

        Ok(Self {
            client,
            fallback_client,
            base_url: base,
            auth_header,
        })
    }

    fn headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(ref auth) = self.auth_header {
            headers.insert(AUTHORIZATION, auth.clone());
        }
        headers
    }

    fn request_url(&self, path: &str) -> Result<Url> {
        self.base_url
            .join(&encode_webdav_path_for_url(path))
            .map_err(Into::into)
    }

    fn curl_request_url(&self, path: &str) -> String {
        let normalized = normalize_standard_path(path);
        let base = self.base_url.as_str().trim_end_matches('/');
        if normalized.starts_with('/') {
            format!("{base}{normalized}")
        } else {
            format!("{base}/{normalized}")
        }
    }

    pub async fn exists(&self, path: &str) -> Result<bool> {
        let url = self.request_url(path)?;
        let mut headers = self.headers();
        headers.insert("Depth", HeaderValue::from_static("0"));

        let resp = self
            .client
            .request(reqwest::Method::from_bytes(b"PROPFIND").unwrap(), url)
            .headers(headers)
            .send()
            .await?;

        Ok(resp.status().is_success())
    }

    pub fn get_stream_url(&self, path: &str) -> Result<String> {
        let mut url = self.request_url(path)?;
        if let Some((user, pass)) = self.get_auth_info()? {
            url.set_username(&user)
                .map_err(|_| anyhow!("Failed to set username"))?;
            url.set_password(Some(&pass))
                .map_err(|_| anyhow!("Failed to set password"))?;
        }
        Ok(url.to_string())
    }

    pub async fn list(&self, path: &str) -> Result<Vec<WebDavResource>> {
        let url = self.request_url(path)?;
        let mut headers = self.headers();
        headers.insert("Depth", HeaderValue::from_static("1"));

        let resp = self
            .client
            .request(reqwest::Method::from_bytes(b"PROPFIND").unwrap(), url)
            .headers(headers)
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(anyhow!("WebDAV PROPFIND failed: {}", resp.status()));
        }

        let body: String = resp.text().await?;
        let multistatus: Multistatus = quick_xml::de::from_str(&body)?;

        let resources = multistatus
            .response
            .into_iter()
            .map(|r| WebDavResource {
                path: r.href,
                is_dir: r
                    .propstat
                    .prop
                    .resource_type
                    .and_then(|rt| rt.collection)
                    .is_some(),
                size: r.propstat.prop.content_length,
            })
            .collect();

        Ok(resources)
    }

    fn get_auth_info(&self) -> Result<Option<(String, String)>> {
        if let Some(ref auth) = self.auth_header {
            let val = auth.to_str()?;
            if val.starts_with("Basic ") {
                let encoded = &val[6..];
                let decoded = String::from_utf8(BASE64_STANDARD.decode(encoded)?)?;
                let parts: Vec<&str> = decoded.splitn(2, ':').collect();
                if parts.len() == 2 {
                    return Ok(Some((parts[0].to_string(), parts[1].to_string())));
                }
            }
        }
        Ok(None)
    }

    async fn send_download_request(
        &self,
        url: Url,
        use_fallback_client: bool,
        range: Option<&str>,
    ) -> Result<reqwest::Response> {
        let mut headers = self.headers();
        let client = if use_fallback_client {
            headers.insert(CONNECTION, HeaderValue::from_static("close"));
            &self.fallback_client
        } else {
            &self.client
        };
        if let Some(range) = range {
            headers.insert(RANGE, HeaderValue::from_str(range)?);
        }
        Ok(client.get(url).headers(headers).send().await?)
    }

    async fn download_with_range_fallback<F>(
        &self,
        path: &str,
        url: Url,
        dest: &Path,
        mut on_progress: F,
    ) -> Result<(u64, String)>
    where
        F: FnMut(u64, Option<u64>),
    {
        use tokio::io::{AsyncWriteExt, BufWriter};

        let probe = self
            .send_download_request(url.clone(), true, Some("bytes=0-0"))
            .await?;
        let probe_status = probe.status();
        let probe_final_url = probe.url().to_string();
        let probe_headers = probe.headers().clone();
        let probe_content_type = header_value(&probe_headers, "content-type");
        let probe_server = header_value(&probe_headers, "server");
        let probe_via = header_value(&probe_headers, "via");
        let probe_accept_ranges = header_value(&probe_headers, "accept-ranges");

        if !probe_status.is_success() {
            return Err(anyhow!(
                "range probe failed for {} (status={}, final_url={}, content_length={:?}, content_type={}, server={}, via={}, accept_ranges={})",
                path,
                probe_status,
                probe_final_url,
                probe.content_length(),
                probe_content_type,
                probe_server,
                probe_via,
                probe_accept_ranges
            ));
        }

        let Some(total_bytes) =
            parse_content_range_total(&probe_headers).or_else(|| probe.content_length())
        else {
            return Err(anyhow!(
                "range probe missing total size for {} (status={}, final_url={}, content_type={}, server={}, via={}, accept_ranges={})",
                path,
                probe_status,
                probe_final_url,
                probe_content_type,
                probe_server,
                probe_via,
                probe_accept_ranges
            ));
        };

        if total_bytes == 0 {
            return Err(anyhow!(
                "range probe reported zero total bytes for {} (status={}, final_url={}, content_type={}, server={}, via={}, accept_ranges={})",
                path,
                probe_status,
                probe_final_url,
                probe_content_type,
                probe_server,
                probe_via,
                probe_accept_ranges
            ));
        }

        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let temp_dest = temp_download_path(dest);
        cleanup_partial_download(&temp_dest).await;
        let file = tokio::fs::File::create(&temp_dest).await?;
        let mut writer = BufWriter::with_capacity(DOWNLOAD_WRITE_BUFFER_SIZE, file);

        let mut total_written = 0u64;
        let mut next_progress_pct = 5u64;
        let mut final_url = probe_final_url.clone();
        let mut start = 0u64;

        while start < total_bytes {
            let end = (start + RANGE_FALLBACK_CHUNK_SIZE - 1).min(total_bytes - 1);
            let expected_chunk_size = end - start + 1;
            let range_value = format!("bytes={start}-{end}");
            let resp = self
                .send_download_request(url.clone(), true, Some(&range_value))
                .await?;
            let status = resp.status();
            let current_final_url = resp.url().to_string();
            let headers = resp.headers().clone();
            let content_type = header_value(&headers, "content-type");
            let server = header_value(&headers, "server");
            let via = header_value(&headers, "via");
            let accept_ranges = header_value(&headers, "accept-ranges");

            if !status.is_success() {
                cleanup_partial_download(&temp_dest).await;
                return Err(anyhow!(
                    "range request failed for {} (range={}, status={}, final_url={}, content_length={:?}, content_type={}, server={}, via={}, accept_ranges={})",
                    path,
                    range_value,
                    status,
                    current_final_url,
                    resp.content_length(),
                    content_type,
                    server,
                    via,
                    accept_ranges
                ));
            }

            let mut stream = resp.bytes_stream();
            let mut chunk_written = 0u64;
            while let Some(chunk) = stream.next().await {
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    Err(e) => {
                        cleanup_partial_download(&temp_dest).await;
                        return Err(anyhow!(
                            "range stream error when downloading {} (range={}, final_url={}, downloaded_bytes={}, total_bytes={}): {}",
                            path,
                            range_value,
                            current_final_url,
                            total_written + chunk_written,
                            total_bytes,
                            e
                        ));
                    }
                };
                chunk_written += chunk.len() as u64;
                total_written += chunk.len() as u64;
                if let Err(e) = writer.write_all(&chunk).await {
                    cleanup_partial_download(&temp_dest).await;
                    return Err(anyhow!(
                        "range write error when downloading {} to {} (range={}, final_url={}, downloaded_bytes={}, total_bytes={}): {}",
                        path,
                        temp_dest.display(),
                        range_value,
                        current_final_url,
                        total_written,
                        total_bytes,
                        e
                    ));
                }

                while next_progress_pct <= 100
                    && total_written.saturating_mul(100)
                        >= total_bytes.saturating_mul(next_progress_pct)
                {
                    on_progress(total_written, Some(total_bytes));
                    next_progress_pct += 5;
                }
            }

            if chunk_written == 0 {
                cleanup_partial_download(&temp_dest).await;
                return Err(anyhow!(
                    "range request returned 0 bytes for {} (range={}, status={}, final_url={}, content_type={}, server={}, via={}, accept_ranges={})",
                    path,
                    range_value,
                    status,
                    current_final_url,
                    content_type,
                    server,
                    via,
                    accept_ranges
                ));
            }

            if !(status == reqwest::StatusCode::PARTIAL_CONTENT
                || (status == reqwest::StatusCode::OK
                    && start == 0
                    && chunk_written == total_bytes))
            {
                cleanup_partial_download(&temp_dest).await;
                return Err(anyhow!(
                    "unexpected status for range request {} (range={}, status={}, final_url={}, chunk_bytes={}, expected_chunk_bytes={})",
                    path,
                    range_value,
                    status,
                    current_final_url,
                    chunk_written,
                    expected_chunk_size
                ));
            }

            if status == reqwest::StatusCode::PARTIAL_CONTENT
                && chunk_written != expected_chunk_size
            {
                cleanup_partial_download(&temp_dest).await;
                return Err(anyhow!(
                    "range size mismatch for {} (range={}, final_url={}, chunk_bytes={}, expected_chunk_bytes={})",
                    path,
                    range_value,
                    current_final_url,
                    chunk_written,
                    expected_chunk_size
                ));
            }

            final_url = current_final_url;
            if status == reqwest::StatusCode::OK {
                break;
            }
            start = start.saturating_add(chunk_written);
        }

        if let Err(e) = writer.flush().await {
            cleanup_partial_download(&temp_dest).await;
            return Err(anyhow!(
                "flush error when downloading {} to {} with range fallback (downloaded_bytes={}, total_bytes={}): {}",
                path,
                temp_dest.display(),
                total_written,
                total_bytes,
                e
            ));
        }

        if total_written != total_bytes {
            cleanup_partial_download(&temp_dest).await;
            return Err(anyhow!(
                "range fallback size mismatch for {} (final_url={}, downloaded_bytes={}, total_bytes={})",
                path,
                final_url,
                total_written,
                total_bytes
            ));
        }

        let file = writer.into_inner();
        #[cfg(unix)]
        {
            file.sync_data().await.ok();
            evict_file_from_page_cache(&file, &temp_dest);
        }
        drop(file);

        tokio::fs::rename(&temp_dest, dest).await?;
        Ok((total_written, final_url))
    }

    async fn download_with_curl_fallback<F>(
        &self,
        path: &str,
        raw_url: &str,
        dest: &Path,
        mut on_progress: F,
    ) -> Result<(u64, String)>
    where
        F: FnMut(u64, Option<u64>),
    {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let temp_dest = temp_download_path(dest);
        cleanup_partial_download(&temp_dest).await;
        let probe_dest = dest.with_file_name(format!(
            "{}.probe",
            dest.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "download".to_string())
        ));
        cleanup_partial_download(&probe_dest).await;

        let mut probe_cmd = Command::new("curl");
        probe_cmd
            .arg("-4")
            .arg("--http1.1")
            .arg("-sS")
            .arg("-L")
            .arg("-H")
            .arg("Range: bytes=0-0")
            .arg("-o")
            .arg(&probe_dest)
            .arg("-w")
            .arg(
                "http_code=%{http_code}\nsize_download=%{size_download}\nurl_effective=%{url_effective}\n",
            );

        if let Some((user, pass)) = self.get_auth_info()? {
            probe_cmd.arg("--user").arg(format!("{user}:{pass}"));
        }

        probe_cmd.arg(raw_url);

        let probe_output = probe_cmd.output().await.map_err(|e| {
            anyhow!(
                "failed to start curl probe for {} (url={}): {}",
                path,
                raw_url,
                e
            )
        })?;

        let (probe_http_code, probe_size_download, probe_url_effective) =
            parse_curl_write_out(&probe_output.stdout);
        let probe_stderr = String::from_utf8_lossy(&probe_output.stderr)
            .trim()
            .to_string();
        let probe_stdout = String::from_utf8_lossy(&probe_output.stdout)
            .trim()
            .to_string();

        if !probe_output.status.success() {
            cleanup_partial_download(&probe_dest).await;
            cleanup_partial_download(&temp_dest).await;
            return Err(anyhow!(
                "curl probe failed for {} (url={}, status={:?}, http_code={:?}, stdout={}, stderr={})",
                path,
                raw_url,
                probe_output.status.code(),
                probe_http_code,
                probe_stdout,
                probe_stderr
            ));
        }

        let probe_meta = tokio::fs::metadata(&probe_dest).await.ok();
        let probe_bytes = probe_meta.map(|m| m.len()).unwrap_or(0);
        cleanup_partial_download(&probe_dest).await;

        if probe_http_code != Some(206) || probe_bytes == 0 {
            cleanup_partial_download(&temp_dest).await;
            return Err(anyhow!(
                "curl probe did not return partial content for {} (url={}, http_code={:?}, size_download={:?}, probe_bytes={}, url_effective={}, stderr={})",
                path,
                raw_url,
                probe_http_code,
                probe_size_download,
                probe_bytes,
                probe_url_effective.unwrap_or_else(|| "-".to_string()),
                probe_stderr
            ));
        }

        let mut cmd = Command::new("curl");
        cmd.arg("-4")
            .arg("--http1.1")
            .arg("-sS")
            .arg("-L")
            .arg("-H")
            .arg("Range: bytes=0-")
            .arg("-o")
            .arg(&temp_dest)
            .arg("-w")
            .arg("http_code=%{http_code}\nsize_download=%{size_download}\nurl_effective=%{url_effective}\n");

        if let Some((user, pass)) = self.get_auth_info()? {
            cmd.arg("--user").arg(format!("{user}:{pass}"));
        }

        cmd.arg(raw_url);

        let output = cmd.output().await.map_err(|e| {
            anyhow!(
                "failed to start curl fallback for {} (url={}): {}",
                path,
                raw_url,
                e
            )
        })?;

        let (http_code, size_download, url_effective) = parse_curl_write_out(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();

        if !output.status.success() {
            cleanup_partial_download(&temp_dest).await;
            return Err(anyhow!(
                "curl fallback failed for {} (url={}, status={:?}, http_code={:?}, stdout={}, stderr={})",
                path,
                raw_url,
                output.status.code(),
                http_code,
                stdout,
                stderr
            ));
        }

        let meta = tokio::fs::metadata(&temp_dest).await.map_err(|e| {
            anyhow!(
                "curl fallback metadata check failed for {} (url={}): {}",
                path,
                raw_url,
                e
            )
        })?;
        let total = meta.len();

        if total == 0 {
            cleanup_partial_download(&temp_dest).await;
            return Err(anyhow!(
                "curl fallback downloaded 0 bytes for {} (url={}, http_code={:?}, size_download={:?}, url_effective={}, stderr={})",
                path,
                raw_url,
                http_code,
                size_download,
                url_effective.clone().unwrap_or_else(|| "-".to_string()),
                stderr
            ));
        }

        let effective_url = url_effective.unwrap_or_else(|| raw_url.to_string());
        on_progress(total, Some(total));

        #[cfg(unix)]
        {
            if let Ok(file) = tokio::fs::File::open(&temp_dest).await {
                file.sync_data().await.ok();
                evict_file_from_page_cache(&file, &temp_dest);
            }
        }

        tokio::fs::rename(&temp_dest, dest).await?;
        Ok((total, effective_url))
    }

    /// Download a remote WebDAV file at `path` to the local `dest` path.
    /// Progress is reported as bytes written to stdout.
    pub async fn download(&self, path: &str, dest: &Path) -> Result<()> {
        self.download_with_progress(path, dest, |_, _| {}).await
    }

    pub async fn download_with_progress<F>(
        &self,
        path: &str,
        dest: &Path,
        mut on_progress: F,
    ) -> Result<()>
    where
        F: FnMut(u64, Option<u64>),
    {
        use tokio::io::{AsyncWriteExt, BufWriter};

        let url = self.request_url(path)?;
        let mut attempt_label = "default";
        let mut resp = self.send_download_request(url.clone(), false, None).await?;
        if resp.status().is_success() && resp.content_length() == Some(0) {
            log::warn!(
                "retrying suspicious zero-length WebDAV response for {} with HTTP/1.1 fallback",
                path
            );
            attempt_label = "http1_fallback";
            resp = self.send_download_request(url.clone(), true, None).await?;
        }
        let status = resp.status();
        let final_url = resp.url().to_string();
        let total_bytes = resp.content_length();
        let headers = resp.headers().clone();
        let content_type = header_value(&headers, "content-type");
        let server = header_value(&headers, "server");
        let via = header_value(&headers, "via");
        let accept_ranges = header_value(&headers, "accept-ranges");

        if !status.is_success() {
            return Err(anyhow!(
                "HTTP {} when downloading {} (final_url={}, content_length={:?}, content_type={}, server={}, via={}, accept_ranges={})",
                status,
                path,
                final_url,
                total_bytes,
                content_type,
                server,
                via,
                accept_ranges
            ));
        }

        if total_bytes == Some(0) {
            let curl_url = self.curl_request_url(path);
            match self
                .download_with_curl_fallback(path, &curl_url, dest, &mut on_progress)
                .await
            {
                Ok((total, fallback_final_url)) => {
                    println!(
                        "Downloaded {} bytes -> {:?} (attempt=curl_fallback, final_url={})",
                        total, dest, fallback_final_url
                    );
                    return Ok(());
                }
                Err(curl_err) => {
                    log::warn!("curl fallback failed for {}: {}", path, curl_err);
                    let (total, fallback_final_url) = self
                        .download_with_range_fallback(path, url, dest, &mut on_progress)
                        .await?;
                    println!(
                        "Downloaded {} bytes -> {:?} (attempt=range_fallback, final_url={})",
                        total, dest, fallback_final_url
                    );
                    return Ok(());
                }
            }
        }

        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Use a 2 MB buffered writer to coalesce many small network chunks
        // into fewer, larger write syscalls – greatly reduces CPU overhead.
        let temp_dest = temp_download_path(dest);
        cleanup_partial_download(&temp_dest).await;
        let file = tokio::fs::File::create(&temp_dest).await?;
        let mut writer = BufWriter::with_capacity(DOWNLOAD_WRITE_BUFFER_SIZE, file);
        let mut stream = resp.bytes_stream();
        let mut total: u64 = 0;
        let mut next_progress_pct: u64 = 5;

        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(e) => {
                    cleanup_partial_download(&temp_dest).await;
                    return Err(anyhow!(
                        "stream error when downloading {} (final_url={}, downloaded_bytes={}, content_length={:?}, content_type={}, server={}, via={}, accept_ranges={}): {}",
                        path,
                        final_url,
                        total,
                        total_bytes,
                        content_type,
                        server,
                        via,
                        accept_ranges,
                        e
                    ));
                }
            };
            total += chunk.len() as u64;
            if let Err(e) = writer.write_all(&chunk).await {
                cleanup_partial_download(&temp_dest).await;
                return Err(anyhow!(
                    "write error when downloading {} to {} (final_url={}, downloaded_bytes={}): {}",
                    path,
                    temp_dest.display(),
                    final_url,
                    total,
                    e
                ));
            }

            if let Some(total_bytes) = total_bytes {
                while next_progress_pct <= 100
                    && total.saturating_mul(100) >= total_bytes.saturating_mul(next_progress_pct)
                {
                    on_progress(total, Some(total_bytes));
                    next_progress_pct += 5;
                }
            }
        }
        if let Err(e) = writer.flush().await {
            cleanup_partial_download(&temp_dest).await;
            return Err(anyhow!(
                "flush error when downloading {} to {} (final_url={}, downloaded_bytes={}): {}",
                path,
                temp_dest.display(),
                final_url,
                total,
                e
            ));
        }

        if total == 0 {
            cleanup_partial_download(&temp_dest).await;
            return Err(anyhow!(
                "HTTP {} returned 0 bytes when downloading {} (final_url={}, content_length={:?}, content_type={}, server={}, via={}, accept_ranges={})",
                status,
                path,
                final_url,
                total_bytes,
                content_type,
                server,
                via,
                accept_ranges
            ));
        }

        if let Some(expected) = total_bytes
            && total != expected
        {
            cleanup_partial_download(&temp_dest).await;
            return Err(anyhow!(
                "download size mismatch for {} (final_url={}, expected_bytes={}, downloaded_bytes={}, content_type={}, server={}, via={}, accept_ranges={})",
                path,
                final_url,
                expected,
                total,
                content_type,
                server,
                via,
                accept_ranges
            ));
        }

        let file = writer.into_inner();
        #[cfg(unix)]
        {
            // Flush dirty pages before hinting DONTNEED so the finished
            // download does not stay hot in page cache until the first reader.
            file.sync_data().await.ok();
            evict_file_from_page_cache(&file, &temp_dest);
        }
        drop(file);

        tokio::fs::rename(&temp_dest, dest).await?;

        if total_bytes.is_none() {
            on_progress(total, None);
        }

        println!(
            "Downloaded {} bytes -> {:?} (attempt={}, final_url={})",
            total, dest, attempt_label, final_url
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::encode_webdav_path_for_url;

    #[test]
    fn encode_webdav_path_for_url_escapes_reserved_path_bytes() {
        assert_eq!(
            encode_webdav_path_for_url("/new/WAAA-630/hhd800.com@WAAA-630.mp4"),
            "/new/WAAA-630/hhd800.com%40WAAA-630.mp4"
        );
    }

    #[test]
    fn encode_webdav_path_for_url_does_not_double_encode_percent_sequences() {
        assert_eq!(
            encode_webdav_path_for_url("/new/WAAA-630/hhd800.com%40WAAA-630.mp4"),
            "/new/WAAA-630/hhd800.com%40WAAA-630.mp4"
        );
    }
}
