use anyhow::Result;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tower_http::cors::CorsLayer;

// ── Server State ─────────────────────────────────────────────────────────────

#[derive(Clone)]
struct AppState {
    data_dir: PathBuf,
    cache: Arc<Mutex<kl::server::KkCache>>,
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

// ── Handlers: kr.json ────────────────────────────────────────────────────────

async fn get_kr(State(state): State<AppState>) -> Response {
    let path = state.kr_path();
    match tokio::fs::read_to_string(&path).await {
        Ok(content) => (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            content,
        )
            .into_response(),
        Err(_) => (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            "{}".to_string(),
        )
            .into_response(),
    }
}

async fn put_kr(State(state): State<AppState>, body: String) -> Response {
    let path = state.kr_path();
    // Validate JSON
    if serde_json::from_str::<serde_json::Value>(&body).is_err() {
        return err_json(StatusCode::BAD_REQUEST, "Invalid JSON");
    }

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }

    match tokio::fs::write(&path, &body).await {
        Ok(_) => {
            println!("[ks] Updated kr.json ({} bytes)", body.len());
            ok_json()
        }
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

// ── Handlers: kwa_db.json ────────────────────────────────────────────────────

async fn get_kwa(State(state): State<AppState>) -> Response {
    let path = state.kwa_path();
    match tokio::fs::read_to_string(&path).await {
        Ok(content) => (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            content,
        )
            .into_response(),
        Err(_) => (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            "{}".to_string(),
        )
            .into_response(),
    }
}

async fn put_kwa(State(state): State<AppState>, body: String) -> Response {
    let path = state.kwa_path();
    if serde_json::from_str::<serde_json::Value>(&body).is_err() {
        return err_json(StatusCode::BAD_REQUEST, "Invalid JSON");
    }

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }

    match tokio::fs::write(&path, &body).await {
        Ok(_) => {
            println!("[ks] Updated kwa_db.json ({} bytes)", body.len());
            ok_json()
        }
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

// ── Handler: /cache (proxy to kl::server logic) ──────────────────────────────

async fn handle_cache(
    State(state): State<AppState>,
    Json(req): Json<kl::server::CacheRequest>,
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
            println!("[ks/cache] Stored: {} ({})", num, title);

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
            eprintln!("[ks/cache] Parse error for {}: {}", req.num, e);
            err_json(StatusCode::UNPROCESSABLE_ENTITY, e.to_string())
        }
    }
}

fn parse_cache_request(req: &kl::server::CacheRequest) -> Result<kr::Movie> {
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
#[command(author, version, about = "KS – central sync server for kk/kl JSON databases")]
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
    use clap::Parser;
    let cli = Cli::parse();

    let data_dir = cli
        .data_dir
        .unwrap_or_else(|| dirs::DIR.config_local_dir().to_path_buf());

    std::fs::create_dir_all(&data_dir).ok();

    let state = AppState {
        data_dir,
        cache: Arc::new(Mutex::new(kl::server::KkCache::load())),
    };

    let app = Router::new()
        .route("/db/kr", get(get_kr).put(put_kr))
        .route("/db/kwa", get(get_kwa).put(put_kwa))
        .route("/cache", post(handle_cache))
        .with_state(state)
        .layer(CorsLayer::permissive());

    let addr = SocketAddr::from(([0, 0, 0, 0], cli.port));
    println!("ks server listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
