use anyhow::Result;
use clap::{Parser, Subcommand};
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
    /// update thumbnail paths in the DB to use existing cache files
    FixThumb,
    /// push both kr.json and kwa_db.json to the ks sync server
    Push {
        /// ks server base URL (overrides config.toml)
        #[arg(short, long)]
        url: Option<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Commands::Cache) => cache(),
        Some(Commands::Dedupe) => dedupe(),
        Some(Commands::DupNum) => dup_num(),
        Some(Commands::FixThumb) => fix_thumb(),
        Some(Commands::Push { url }) => push(url),
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
    let cache_dir = &*dirs::THUMB_CACHE_DIR;

    db.config
        .movies
        .clone()
        .iter()
        .enumerate()
        .filter_map(|(i, movie)| {
            Some((
                i,
                movie.abs_path().parent()?.join(movie.movie.thumb.clone()?),
            ))
        })
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
            let dst = cache_dir.join(&filename);
            if dst.exists() {
                return;
            }

            if std::fs::copy(&path, &dst).is_err() {
                println!("{:?} copy failed", path);
                return;
            };

            db.config.movies[i].movie.thumb = Some(filename);
        });

    db.flush();
    Ok(())
}

fn dup_num() -> Result<()> {
    let db = SimpleJsonDatabase::new()?;
    let mut num_map: HashMap<String, Vec<String>> = HashMap::new();

    for movie in &db.config.movies {
        let abs_path = movie.abs_path();
        let Some(stem) = abs_path.file_stem().and_then(|s| s.to_str()) else {
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

        num_map
            .entry(stem_lower)
            .or_default()
            .push(abs_path.to_string_lossy().to_string());
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

fn fix_thumb() -> Result<()> {
    let mut db = SimpleJsonDatabase::new()?;
    let cache_dir = &*dirs::THUMB_CACHE_DIR;
    let mut updated_count = 0;

    for movie in db.config.movies.iter_mut() {
        let Some(ref thumb) = movie.movie.thumb.clone() else {
            continue;
        };

        // Determine the folder name (番號) from the NFO path
        let abs_path = movie.abs_path();
        let Some(folder_name) = abs_path.parent().and_then(|p| p.file_name()) else {
            continue;
        };
        let folder_name = folder_name.to_string_lossy();

        // Get extension from the current thumb value
        let thumb_path = std::path::Path::new(thumb);
        let Some(ext) = thumb_path.extension().and_then(|e| e.to_str()) else {
            continue;
        };

        // Build the expected cache filename
        let cache_filename = format!("{}.{}", folder_name, ext);
        let cache_path = cache_dir.join(&cache_filename);

        // Skip if already pointing to the cache file
        if thumb == &cache_filename {
            continue;
        }

        // Update only if the cache file already exists
        if cache_path.exists() {
            println!(
                "Updating thumb for {:?}: {} -> {}",
                abs_path, thumb, cache_filename
            );
            movie.movie.thumb = Some(cache_filename);
            updated_count += 1;
        }
    }

    if updated_count > 0 {
        db.flush();
        println!(
            "Updated {} thumbnail path(s). Database saved.",
            updated_count
        );
    } else {
        println!("No thumbnail paths needed updating.");
    }

    Ok(())
}

fn push(url: Option<String>) -> Result<()> {
    let base_url = url.or_else(dirs::ks_base_url).ok_or_else(|| {
        anyhow::anyhow!("No ks base URL provided. Pass --url or set ks.base_url in config.toml")
    })?;

    kr::sync::push_kr(&base_url)?;
    kr::sync::push_kwa(&base_url)?;

    Ok(())
}
