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
    /// remove duplicate nfo path records
    Dedupe,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Commands::Cache) => cache(),
        Some(Commands::Dedupe) => dedupe(),
        _ => Ok(()),
    }
}

fn dedupe() -> Result<()> {
    let mut db = SimpleJsonDatabase::new()?;
    let original_len = db.config.movies.len();
    println!("Total records before dedupe: {}", original_len);

    let mut seen = std::collections::HashSet::new();
    let mut unique_movies = Vec::with_capacity(original_len);
    let mut removed_count = 0;

    for movie in db.config.movies.drain(..) {
        if seen.insert(movie.path.clone()) {
            unique_movies.push(movie);
        } else {
            removed_count += 1;
            println!("Duplicate removed: {:?}", movie.path);
        }
    }

    if removed_count > 0 {
        db.config.movies = unique_movies;
        db.flush();
        println!(
            "Removed {} duplicate records. Database saved.",
            removed_count
        );
    } else {
        println!("No duplicates found.");
    }

    Ok(())
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

            let name = path
                .parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_str()
                .unwrap();
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
