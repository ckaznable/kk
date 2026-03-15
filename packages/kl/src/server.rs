use anyhow::Result;
use kr::Movie;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
};

// ── Cache on-disk format ─────────────────────────────────────────────────────

/// A single entry in `kk_cache.json`: the scraped metadata keyed by
/// the normalised movie number (upper-case, hyphens removed).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct KkCache {
    /// Maps normalised movie-number → Movie metadata.
    pub movies: HashMap<String, Movie>,
}

impl KkCache {
    fn cache_path() -> PathBuf {
        dirs::DIR.config_local_dir().join("kk_cache.json")
    }

    /// Load the cache from disk (returns an empty cache on any error).
    pub fn load() -> Self {
        let path = Self::cache_path();
        if !path.exists() {
            return Self::default();
        }
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Look up a movie by its number (comparison is normalised).
    pub fn find(&self, number: &str) -> Option<&Movie> {
        let key = normalise_key(number);
        self.movies.get(&key)
    }

    /// Insert / overwrite an entry and immediately persist to disk.
    pub fn insert_and_flush(&mut self, number: &str, movie: Movie) {
        let key = normalise_key(number);
        self.movies.insert(key, movie);
        self.flush();
    }

    pub fn flush(&self) {
        let path = Self::cache_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        if let Ok(content) = serde_json::to_string(self) {
            std::fs::write(path, content).ok();
        }
    }
}

/// Normalise a movie-number key: upper-case, strip hyphens and underscores.
fn normalise_key(number: &str) -> String {
    number.to_uppercase().replace('-', "").replace('_', "")
}

/// If the number has no dash between the alphabetic prefix and the numeric
/// suffix, insert one automatically (e.g. `"SSIS123"` → `"SSIS-123"`).
/// IDs that already contain a dash, or non-standard formats, are returned
/// unchanged.
fn ensure_dash(number: &str) -> String {
    // Already has a dash → nothing to do.
    if number.contains('-') {
        return number.to_string();
    }

    // Find the boundary where letters end and digits begin.
    let boundary = number
        .char_indices()
        .find(|(_, c)| c.is_ascii_digit())
        .map(|(i, _)| i);

    match boundary {
        // There is a letter prefix followed by digits → insert dash.
        Some(i) if i > 0 => format!("{}-{}", &number[..i], &number[i..]),
        // Pure digits, pure letters, or empty → leave as-is.
        _ => number.to_string(),
    }
}

// ── HTTP request/response types ──────────────────────────────────────────────

#[derive(Deserialize, Debug)]
pub struct CacheRequest {
    /// Scraper type: "javdb" or "fc2".
    #[serde(rename = "type")]
    pub scraper_type: String,
    /// The movie number / 番號 (e.g. "SSIS-123" or "FC2-12345").
    pub num: String,
    /// Raw HTML from the detail page.
    pub html: String,
}

#[derive(Serialize)]
struct ApiOk {
    ok: bool,
    num: String,
    title: String,
}

#[derive(Serialize)]
struct ApiErr {
    ok: bool,
    error: String,
}

// ── Server entry-point ───────────────────────────────────────────────────────

/// Start the HTTP server on `port` (default 6969).
/// Blocks until the process is killed.
pub async fn run_server(port: u16) -> Result<()> {
    use axum::{routing::post, Router};
    use tower_http::cors::CorsLayer;

    let cache = Arc::new(Mutex::new(KkCache::load()));

    let app = Router::new()
        .route("/cache", post(handle_cache))
        .with_state(cache)
        .layer(CorsLayer::permissive());

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    println!("kl cache server listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

// ── Handler ──────────────────────────────────────────────────────────────────

async fn handle_cache(
    axum::extract::State(cache): axum::extract::State<Arc<Mutex<KkCache>>>,
    axum::Json(mut req): axum::Json<CacheRequest>,
) -> axum::response::Response {
    use axum::{http::StatusCode, response::IntoResponse, Json};

    // Auto-insert a dash between the alphabetic prefix and numeric suffix when
    // the caller sends an ID without one (e.g. "SSIS123" → "SSIS-123").
    req.num = ensure_dash(&req.num);

    let result = parse_request(&req);

    match result {
        Ok(movie) => {
            let num = movie.num.clone().unwrap_or_else(|| req.num.clone());
            let title = movie.title.clone();
            {
                let mut c = cache.lock().unwrap();
                c.insert_and_flush(&req.num, movie);
            }
            println!("[cache] Stored: {} ({})", num, title);
            (
                StatusCode::OK,
                Json(ApiOk {
                    ok: true,
                    num,
                    title,
                }),
            )
                .into_response()
        }
        Err(e) => {
            eprintln!("[cache] Parse error for {}: {}", req.num, e);
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ApiErr {
                    ok: false,
                    error: e.to_string(),
                }),
            )
                .into_response()
        }
    }
}

fn parse_request(req: &CacheRequest) -> Result<Movie> {
    match req.scraper_type.to_lowercase().as_str() {
        "javdb" => {
            let scraper = crate::javdb::JavdbScraper::new()?;
            scraper.parse_html(&req.num, &req.html)
        }
        "fc2" => {
            let scraper = crate::fc2::Fc2Scraper::new()?;
            let parser = scraper.parse_html(&req.num);
            parser(&req.html)
        }
        other => anyhow::bail!("Unknown scraper type: {}", other),
    }
}
