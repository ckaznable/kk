use anyhow::Result;
use clap::{Parser, Subcommand};
use dirs::DIR;
use kr::db::SimpleJsonDatabase;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// cache data in config
    Cache,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Commands::Cache) => cache(),
        _ => Ok(())
    }
}

fn cache() -> Result<()> {
    let mut db = SimpleJsonDatabase::new()?;
    let cache_dir = DIR.cache_dir().join("thumbs");
    std::fs::create_dir_all(&cache_dir).unwrap();

    db.config
        .movies
        .clone()
        .iter()
        .enumerate()
        .filter_map(|(i, movie)| Some((i, movie.path.parent()?.join(movie.movie.thumb.clone()?))))
        .for_each(|(i, path)| {
            let Some(ext) = path.extension() else {
                println!("{:?} ext name not found", path);
                return;
            };

            let name = path.parent().unwrap().file_name().unwrap().to_str().unwrap();
            let filename = format!("{}.{}", name, ext.to_str().unwrap());
            let dst = cache_dir.join(filename);
            if dst.exists() {
                return;
            }

            if std::fs::copy(&path, &dst).is_err() {
                println!("{:?} copy failed", path);
                return;
            };

            db.config.movies[i].movie.thumb = Some(dst.to_string_lossy().to_string());
        });

    db.flush();
    Ok(())
}