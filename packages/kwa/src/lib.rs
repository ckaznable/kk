use anyhow::{Result, anyhow};
use base64::prelude::*;
use futures_util::StreamExt;
use reqwest::Client;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::path::Path;
use url::Url;

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

#[derive(Debug, Clone)]
pub struct WebDavClient {
    client: Client,
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

        Ok(Self {
            client: Client::new(),
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

    pub async fn exists(&self, path: &str) -> Result<bool> {
        let url = self.base_url.join(path)?;
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
        let mut url = self.base_url.join(path)?;
        if let Some((user, pass)) = self.get_auth_info()? {
            url.set_username(&user)
                .map_err(|_| anyhow!("Failed to set username"))?;
            url.set_password(Some(&pass))
                .map_err(|_| anyhow!("Failed to set password"))?;
        }
        Ok(url.to_string())
    }

    pub async fn list(&self, path: &str) -> Result<Vec<WebDavResource>> {
        let url = self.base_url.join(path)?;
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

        let url = self.base_url.join(path)?;
        let resp = self.client.get(url).headers(self.headers()).send().await?;

        if !resp.status().is_success() {
            return Err(anyhow!("HTTP {} when downloading {}", resp.status(), path));
        }

        let total_bytes = resp.content_length();

        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Use a 2 MB buffered writer to coalesce many small network chunks
        // into fewer, larger write syscalls – greatly reduces CPU overhead.
        const BUF_SIZE: usize = 2 * 1024 * 1024;
        let file = tokio::fs::File::create(dest).await?;
        let mut writer = BufWriter::with_capacity(BUF_SIZE, file);
        let mut stream = resp.bytes_stream();
        let mut total: u64 = 0;
        let mut next_progress_pct: u64 = 5;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            total += chunk.len() as u64;
            writer.write_all(&chunk).await?;

            if let Some(total_bytes) = total_bytes {
                while next_progress_pct <= 100
                    && total.saturating_mul(100) >= total_bytes.saturating_mul(next_progress_pct)
                {
                    on_progress(total, Some(total_bytes));
                    next_progress_pct += 5;
                }
            }
        }
        writer.flush().await?;
        let file = writer.into_inner();
        #[cfg(unix)]
        {
            // Flush dirty pages before hinting DONTNEED so the finished
            // download does not stay hot in page cache until the first reader.
            file.sync_data().await.ok();
            evict_file_from_page_cache(&file, dest);
        }

        if total_bytes.is_none() {
            on_progress(total, None);
        }

        println!("Downloaded {} bytes -> {:?}", total, dest);
        Ok(())
    }
}
