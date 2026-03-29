use anyhow::{Result, anyhow};
use base64::prelude::*;
use futures_util::StreamExt;
use reqwest::Client;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use std::path::Path;
use url::Url;

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
        use tokio::io::{AsyncWriteExt, BufWriter};

        let url = self.base_url.join(path)?;
        let resp = self.client.get(url).headers(self.headers()).send().await?;

        if !resp.status().is_success() {
            return Err(anyhow!("HTTP {} when downloading {}", resp.status(), path));
        }

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

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            total += chunk.len() as u64;
            writer.write_all(&chunk).await?;
        }
        writer.flush().await?;

        println!("Downloaded {} bytes -> {:?}", total, dest);
        Ok(())
    }
}
