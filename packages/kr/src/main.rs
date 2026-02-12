use anyhow::Result;
use clap::{Parser, Subcommand};
use dirs::DIR;
use kr::db::SimpleJsonDatabase;
use std::collections::HashMap;

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
    /// find movies with duplicate num (番號) and list their absolute paths
    DupNum,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Commands::Cache) => cache(),
        Some(Commands::Dedupe) => dedupe(),
        Some(Commands::DupNum) => dup_num(),
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

fn dup_num() -> Result<()> {
    let db = SimpleJsonDatabase::new()?;
    let mut num_map: HashMap<String, Vec<String>> = HashMap::new();

    for movie in &db.config.movies {
        let Some(stem) = movie.path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };

        let stem_lower = stem.to_lowercase();

        // Skip multi-disc entries (filename ending with -cd1, _CD2, etc.)
        if let Some(pos) = stem_lower.rfind("cd") {
            let after_cd = &stem_lower[pos + 2..];
            if !after_cd.is_empty() && after_cd.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
        }

        let abs_path = if movie.path.is_absolute() {
            movie.path.to_string_lossy().to_string()
        } else {
            std::fs::canonicalize(&movie.path)
                .unwrap_or(movie.path.clone())
                .to_string_lossy()
                .to_string()
        };

        num_map.entry(stem_lower).or_default().push(abs_path);
    }

    let mut dup_count = 0;
    let mut sorted: Vec<_> = num_map
        .iter()
        .filter(|(_, paths)| paths.len() > 1)
        .collect();
    sorted.sort_by(|(a, _), (b, _)| a.cmp(b));

    for (num, paths) in &sorted {
        dup_count += 1;
        println!("[{}] ({} duplicates)", num, paths.len());
        for p in *paths {
            println!("  {}", p);
        }
        println!();
    }

    if dup_count == 0 {
        println!("No duplicate numbers found.");
    } else {
        println!("Found {} duplicate number(s).", dup_count);
    }

    Ok(())
}
