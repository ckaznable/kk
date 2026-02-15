use clap::{Parser, Subcommand};
use kwa::WebDavClient;
use anyhow::Result;

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
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let url = cli.url.or_else(|| dirs::WEBDAV_URL.clone()).ok_or_else(|| anyhow::anyhow!("WebDAV URL not provided via --url or WEBDAV_URL env"))?;
    let user = cli.user.or_else(|| dirs::WEBDAV_USER.clone());
    let pass = cli.pass.or_else(|| dirs::WEBDAV_PASS.clone());

    let auth = if let (Some(u), Some(p)) = (user, pass) {
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
                    let size_str = res.size.map(|s| s.to_string()).unwrap_or_else(|| "-".to_string());
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
    }

    Ok(())
}
