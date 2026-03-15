use anyhow::Result;
use clap::{Parser, Subcommand};
use kwa::WebDavClient;
use std::path::PathBuf;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// WebDAV base URL
    #[arg(long)]
    url: Option<String>,

    /// WebDAV username
    #[arg(short, long)]
    user: Option<String>,

    /// WebDAV password
    #[arg(short, long)]
    pass: Option<String>,

    /// Output in JSON format
    #[arg(long)]
    json: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// List files in a path
    List {
        /// Path to list
        #[arg(default_value = "/")]
        path: String,
    },
    /// Check if a path exists
    Exists {
        /// Path to check
        path: String,
    },
    /// Get an authenticated stream URL for a path
    StreamUrl {
        /// Path to the file
        path: String,
    },
    /// Download all items marked as pending download from kwa_db to a local directory
    Download {
        /// Local directory to save downloaded files into
        output: PathBuf,

        /// Dry run: list pending items without downloading
        #[arg(long)]
        dry_run: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // For the Download subcommand the client credentials are optional if they
    // are stored in the database / env already.
    let url_opt = cli.url.or_else(|| dirs::WEBDAV_URL.clone());
    let user_opt = cli.user.or_else(|| dirs::WEBDAV_USER.clone());
    let pass_opt = cli.pass.or_else(|| dirs::WEBDAV_PASS.clone());

    match cli.command {
        Commands::Download { output, dry_run } => {
            return run_download(url_opt, user_opt, pass_opt, output, dry_run, cli.json).await;
        }
        _ => {}
    }

    let url = url_opt
        .ok_or_else(|| anyhow::anyhow!("WebDAV URL not provided via --url or WEBDAV_URL env"))?;

    let auth = if let (Some(u), Some(p)) = (user_opt, pass_opt) {
        Some((u, p))
    } else {
        None
    };

    let client = WebDavClient::new(&url, auth)?;

    match cli.command {
        Commands::List { path } => {
            let resources = client.list(&path).await?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&resources)?);
            } else {
                for res in resources {
                    let type_str = if res.is_dir { "DIR " } else { "FILE" };
                    let size_str = res
                        .size
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "-".to_string());
                    println!("{} {:>10} {}", type_str, size_str, res.path);
                }
            }
        }
        Commands::Exists { path } => {
            let exists = client.exists(&path).await?;
            if cli.json {
                println!("{}", serde_json::json!({ "path": path, "exists": exists }));
            } else {
                if exists {
                    println!("Path exists: {}", path);
                } else {
                    println!("Path does not exist: {}", path);
                }
            }
        }
        Commands::StreamUrl { path } => {
            let url = client.get_stream_url(&path)?;
            if cli.json {
                println!("{}", serde_json::json!({ "path": path, "stream_url": url }));
            } else {
                println!("{}", url);
            }
        }
        Commands::Download { .. } => unreachable!(),
    }

    Ok(())
}

async fn run_download(
    url: Option<String>,
    user: Option<String>,
    pass: Option<String>,
    output: PathBuf,
    dry_run: bool,
    json: bool,
) -> Result<()> {
    let mut db = kr::db::WebDavDatabase::new()?;

    // Override credentials if provided on the CLI / env
    if let Some(u) = url {
        db.config.base_url = u;
    }
    if user.is_some() {
        db.config.user = user;
    }
    if pass.is_some() {
        db.config.pass = pass;
    }

    if db.config.base_url.is_empty() {
        return Err(anyhow::anyhow!(
            "WebDAV URL is not set. Provide it via --url or KK_WEBDAV_URL env var."
        ));
    }

    let auth = db.config.user.clone().zip(db.config.pass.clone());
    let client = WebDavClient::new(&db.config.base_url, auth)?;

    // Collect indices of pending items first so we can mutate db later
    let pending_indices: Vec<usize> = db
        .config
        .movies
        .iter()
        .enumerate()
        .filter(|(_, m)| m.pending_download)
        .map(|(i, _)| i)
        .collect();

    if pending_indices.is_empty() {
        println!("No items marked for download.");
        return Ok(());
    }

    println!("{} item(s) pending download.", pending_indices.len());

    if !output.exists() {
        std::fs::create_dir_all(&output)?;
    }

    let mut completed = vec![];

    for &idx in &pending_indices {
        let movie = &db.config.movies[idx];
        let url_path = movie.url_path.clone();
        let filename = std::path::Path::new(&url_path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| format!("download_{}", idx));

        let dest = output.join(&filename);

        if json {
            println!(
                "{}",
                serde_json::json!({
                    "index": idx,
                    "url_path": url_path,
                    "dest": dest.to_string_lossy(),
                    "dry_run": dry_run
                })
            );
        } else {
            println!("[{}] {} -> {:?}", idx, url_path, dest);
        }

        if dry_run {
            continue;
        }

        match client.download(&url_path, &dest).await {
            Ok(()) => {
                println!("  OK");
                completed.push(idx);
            }
            Err(e) => {
                eprintln!("  FAILED: {}", e);
            }
        }
    }

    // Clear pending_download flag for successfully downloaded items and persist
    if !completed.is_empty() {
        for idx in completed {
            db.config.movies[idx].pending_download = false;
        }
        db.flush();
        println!("Database updated: cleared pending_download flags.");
    }

    Ok(())
}
