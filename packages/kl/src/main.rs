use clap::{Parser, Subcommand};
use kl::Scraper;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::{thread, time::Duration};
use walkdir::WalkDir;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Scrape and organize files
    Tidy {
        #[arg(short, long)]
        input: PathBuf,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Fix database issues like broken thumbnails
    FixDb {
        /// Perform a trial run without changes
        #[arg(long)]
        dry_run: bool,

        /// Test fix on the first item found (execute scrape/download) but do not save changes
        #[arg(long)]
        test_first: bool,

        /// Limit the number of items to fix (and save)
        #[arg(long)]
        fix_num: Option<usize>,

        /// List items that need fixing without taking action
        #[arg(long)]
        list_need_fix: bool,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::FixDb {
            dry_run,
            test_first,
            fix_num,
            list_need_fix,
        } => {
            run_fix_db(dry_run, test_first, fix_num, list_need_fix)?;
        }
        Commands::Tidy { input, output } => {
            let output_path = output.unwrap_or_else(|| dirs::SEARCH_PATH.to_path_buf());
            run_scraper(input, output_path)?;
        }
    }
    Ok(())
}

fn run_fix_db(
    dry_run: bool,
    test_first: bool,
    fix_num: Option<usize>,
    list_need_fix: bool,
) -> anyhow::Result<()> {
    // Explicitly load DB configuration instead of using default empty init
    let mut db = kr::db::SimpleJsonDatabase::new()
        .map_err(|e| anyhow::anyhow!("Failed to load database: {}", e))?;
    let javdb = kl::javdb::JavdbScraper::new()?;
    let fc2 = kl::fc2::Fc2Scraper::new()?;

    let mut modified = false;
    let mut fix_count = 0;
    let mut found_issues_count = 0;

    let len = db.config.movies.len();
    println!(
        "Database path: {:?}",
        dirs::DIR.config_local_dir().join("kr.json")
    );
    println!("Loaded {} movies from database.", len);

    let cache_dir = dirs::DIR.cache_dir().join("thumbs");
    if !cache_dir.exists() {
        fs::create_dir_all(&cache_dir)?;
    }
    // println!("Cache thumbs dir: {:?}", cache_dir); // Valid but reduces noise for list mode?

    for i in 0..len {
        let mut needs_rescrape = false;
        // Analyze current state
        let mut target_download_url: Option<String> = None;

        // Information for reporting
        let movie_title = db.config.movies[i].movie.title.clone();
        let movie_num_opt = db.config.movies[i].movie.num.clone();

        // Extract ID for cache filename
        let file_stem_for_id;
        {
            // We need to extract file stem early because borrow checker
            let p = &db.config.movies[i].path;
            file_stem_for_id = p.file_stem().unwrap().to_string_lossy().to_string();
        }

        // Try getting ID from movie.num, fallback to file_stem
        let movie_num = movie_num_opt.clone();
        let cache_id = movie_num
            .clone()
            .unwrap_or_else(|| file_stem_for_id.clone());
        let cache_thumb_path = cache_dir.join(format!("{}.jpg", cache_id)); // Using .jpg by convention

        {
            if let Some(movie_data) = db.config.movies.get(i) {
                if let Some(thumb) = &movie_data.movie.thumb {
                    if thumb.starts_with("http") {
                        target_download_url = Some(thumb.clone());
                    } else if thumb.ends_with(".com") || thumb.contains(".com/") {
                        // println!("Suspicious thumb (URL-like): {}", thumb);
                        if thumb.contains("/") {
                            target_download_url = Some(if thumb.starts_with("//") {
                                format!("https:{}", thumb)
                            } else {
                                format!("https://{}", thumb)
                            });
                        } else {
                            needs_rescrape = true;
                        }
                    } else {
                        // Check local
                        let p = PathBuf::from(thumb);
                        if !p.exists() {
                            // println!("Missing local thumb: {:?}", p);
                            needs_rescrape = true;
                        } else {
                            if let Some(ext) = p.extension() {
                                let ext_s = ext.to_string_lossy().to_lowercase();
                                if !["jpg", "jpeg", "png", "gif", "webp"].contains(&ext_s.as_str())
                                {
                                    // println!("Invalid thumb extension: {:?}", p);
                                    needs_rescrape = true;
                                }
                            } else {
                                // println!("No extension for thumb: {:?}", p);
                                needs_rescrape = true;
                            }
                        }
                    }
                } else {
                    // No thumb
                }
            }
        }

        let display_id = movie_num.unwrap_or_else(|| "Unknown".to_string());

        if list_need_fix {
            if let Some(url) = target_download_url {
                println!(
                    "[Fix Needed] ID: {:<12} | Title: {:.30} | Action: Download URL ({})",
                    display_id, movie_title, url
                );
                found_issues_count += 1;
                continue;
            }
            if needs_rescrape {
                println!("[Fix Needed] ID: {:<12} | Title: {:.30} | Action: Re-scrape (Invalid/Missing Thumb)", display_id, movie_title);
                found_issues_count += 1;
                continue;
            }
            // If neither, just continue loop
            continue;
        }

        let nfo_path = db.config.movies[i].path.clone();
        let parent = nfo_path.parent().unwrap();
        let file_stem = nfo_path.file_stem().unwrap().to_string_lossy();
        let target_img_path = parent.join(format!("{}-poster.jpg", file_stem));

        // Attempt download if we have a URL and don't need full rescrape yet
        if let Some(url) = target_download_url {
            if dry_run && !test_first {
                println!("  [Dry Run] Would download thumb: {}", url);
            } else {
                println!("  Downloading thumb: {}", url);

                let dest = if test_first {
                    std::env::temp_dir().join(format!("kl_test_{}.jpg", file_stem))
                } else {
                    target_img_path.clone()
                };

                match download_file(&url, &dest) {
                    Ok(_) => {
                        println!("  Downloaded successfully to: {:?}", dest);
                        if test_first {
                            println!("  [Test First] Download verified. Stopping.");
                            return Ok(());
                        }

                        // Copy to Cache
                        if let Err(e) = fs::copy(&dest, &cache_thumb_path) {
                            println!("  Failed to copy to cache: {}", e);
                        } else {
                            println!("  Cached to: {:?}", cache_thumb_path);
                        }

                        // Update DB points to CACHE for thumb
                        db.config.movies[i].movie.thumb =
                            Some(cache_thumb_path.to_string_lossy().to_string());

                        // Update poster to local NFO side file
                        db.config.movies[i].movie.poster =
                            Some(target_img_path.to_string_lossy().to_string());

                        modified = true;
                        fix_count += 1;

                        update_nfo_file(&db.config.movies[i]);

                        if let Some(limit) = fix_num {
                            if fix_count >= limit {
                                println!("Reached fix limit of {}. Breaking loop.", limit);
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        println!("  Download failed ({}): {}", url, e);
                        needs_rescrape = true;
                    }
                }
            }
        }

        // Check if we broke out of loop in download match
        if let Some(limit) = fix_num {
            if fix_count >= limit {
                break;
            }
        }

        if needs_rescrape {
            if dry_run && !test_first {
                println!(
                    "  [Dry Run] Would re-scrape: {:?}",
                    db.config.movies[i].movie.title
                );
                continue;
            }

            let id_opt = db.config.movies[i].movie.num.clone();
            if let Some(id) = id_opt {
                println!("  Re-scraping ID: {}", id);
                let scraper: &dyn Scraper = if id.to_uppercase().starts_with("FC2") {
                    &fc2
                } else {
                    &javdb
                };

                match scraper.scrape(&id) {
                    Ok(mut new_movie) => {
                        println!("  Scraped: {}", new_movie.title);

                        if test_first {
                            println!("  [Test First] Scrape successful.");
                            // ... check logic ...
                            println!("  stopping.");
                            return Ok(());
                        }

                        // Handle images
                        if let Some(poster_url) = &new_movie.poster {
                            if let Ok(_) = download_file(poster_url, &target_img_path) {
                                println!("  Downloaded new thumb: {:?}", target_img_path);

                                // Copy to Cache
                                if let Err(e) = fs::copy(&target_img_path, &cache_thumb_path) {
                                    println!("  Failed to copy to cache: {}", e);
                                    // Fallback to local?
                                    new_movie.thumb =
                                        Some(target_img_path.to_string_lossy().to_string());
                                } else {
                                    println!("  Cached to: {:?}", cache_thumb_path);
                                    // Point thumb to cache
                                    new_movie.thumb =
                                        Some(cache_thumb_path.to_string_lossy().to_string());
                                }

                                new_movie.poster =
                                    Some(target_img_path.to_string_lossy().to_string());
                            }
                        }

                        db.config.movies[i].movie = new_movie.clone();
                        update_nfo_file(&db.config.movies[i]);

                        modified = true;
                        fix_count += 1;
                        if let Some(limit) = fix_num {
                            if fix_count >= limit {
                                println!("Reached fix limit of {}. Breaking loop.", limit);
                                break;
                            }
                        }
                    }
                    Err(e) => eprintln!("  Re-scrape failed: {}", e),
                }

                thread::sleep(Duration::from_secs(2));
            }
        }
    }

    if list_need_fix {
        println!("Total items needing fix: {}", found_issues_count);
        return Ok(());
    }

    if modified {
        db.flush();
        println!("Database flushed with {} fixes.", fix_count);
    } else {
        println!("No changes made.");
    }

    Ok(())
}

fn download_file(url: &str, path: &PathBuf) -> anyhow::Result<()> {
    let bytes = reqwest::blocking::get(url)?.bytes()?;
    let mut file = fs::File::create(path)?;
    file.write_all(&bytes)?;
    Ok(())
}

fn update_nfo_file(movie_data: &kr::db::MovieData) {
    if let Ok(xml) = kl::generate_nfo_xml(&movie_data.movie) {
        let _ = fs::write(&movie_data.path, xml);
    }
}

fn run_scraper(input: PathBuf, output: PathBuf) -> anyhow::Result<()> {
    if !output.exists() {
        fs::create_dir_all(&output)?;
    }

    let cache_dir = dirs::DIR.cache_dir().join("thumbs");
    if !cache_dir.exists() {
        fs::create_dir_all(&cache_dir)?;
    }

    // Load database
    let mut db = kr::db::SimpleJsonDatabase::new()
        .map_err(|e| anyhow::anyhow!("Failed to load database: {}", e))?;

    let javdb = kl::javdb::JavdbScraper::new()?;
    let fc2 = kl::fc2::Fc2Scraper::new()?;

    for entry in WalkDir::new(input).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_file() {
            if let Some(ext) = path.extension() {
                let ext_str = ext.to_string_lossy().to_lowercase();
                if ["mp4", "mkv", "avi", "wmv", "mov", "iso", "rmvb"].contains(&ext_str.as_str()) {
                    println!("Processing: {:?}", path);
                    if let Some(number) = kl::number_parser::get_number(path) {
                        println!("  Found ID: {}", number);

                        let filename_starts_fc2 = path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_lowercase().starts_with("fc2"))
                            .unwrap_or(false);

                        let scraper: &dyn Scraper =
                            if number.to_uppercase().starts_with("FC2") || filename_starts_fc2 {
                                &fc2
                            } else {
                                &javdb
                            };

                        match scraper.scrape(&number) {
                            Ok(mut movie) => {
                                println!("  Scraped: {}", movie.title);

                                // Determine output directory based on actor count
                                let movie_dir = if movie.actor.len() == 1 {
                                    let actor_name =
                                        movie.actor[0].name.replace("/", "_").replace("\\", "_");
                                    output.join(actor_name).join(&number)
                                } else {
                                    output.join(&number)
                                };

                                if !movie_dir.exists() {
                                    fs::create_dir_all(&movie_dir)?;
                                }

                                // 1. Move video file
                                let ext = path.extension().unwrap_or_default().to_string_lossy();
                                let video_filename = format!("{}.{}", number, ext);
                                let video_dest = movie_dir.join(video_filename);
                                match fs::rename(path, &video_dest) {
                                    Ok(_) => println!("  Moved video to: {:?}", video_dest),
                                    Err(e) => eprintln!("  Failed to move video: {}", e),
                                }

                                // 2. Save NFO
                                let nfo_path = movie_dir.join(format!("{}.nfo", number));
                                match kl::generate_nfo_xml(&movie) {
                                    Ok(xml) => {
                                        let mut file = fs::File::create(&nfo_path)?;
                                        file.write_all(xml.as_bytes())?;
                                        println!("  Saved NFO: {:?}", nfo_path);
                                    }
                                    Err(e) => eprintln!("  Error generating NFO: {}", e),
                                }

                                // 3. Download images
                                let global_thumb_path = cache_dir.join(format!("{}.jpg", number));

                                if let Some(poster_url) = &movie.poster {
                                    // Download poster to movie dir
                                    let img_path = movie_dir.join(format!("{}-poster.jpg", number));
                                    match reqwest::blocking::get(poster_url) {
                                        Ok(response) => {
                                            if let Ok(bytes) = response.bytes() {
                                                if let Ok(mut file) = fs::File::create(&img_path) {
                                                    file.write_all(&bytes).ok();
                                                    println!("  Saved Poster: {:?}", img_path);

                                                    // Copy to global cache
                                                    if let Err(e) =
                                                        fs::write(&global_thumb_path, &bytes)
                                                    {
                                                        eprintln!(
                                                            "  Failed to write to cache: {}",
                                                            e
                                                        );
                                                    } else {
                                                        println!(
                                                            "  Cached thumb: {:?}",
                                                            global_thumb_path
                                                        );
                                                        // Update movie thumb to point to cache
                                                        movie.thumb = Some(
                                                            global_thumb_path
                                                                .to_string_lossy()
                                                                .to_string(),
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                        Err(e) => eprintln!("  Failed to download poster: {}", e),
                                    }
                                }

                                // Add to database
                                let movie_data = kr::db::MovieData {
                                    path: nfo_path,
                                    movie,
                                    added_time: std::time::SystemTime::now(),
                                    fav: false,
                                };
                                db.config.movies.push(movie_data);
                            }
                            Err(e) => {
                                eprintln!("  Scrape failed for {}: {}", number, e);
                            }
                        }

                        // Add delay to avoid rate limiting
                        thread::sleep(Duration::from_secs(2));
                    } else {
                        println!("  Could not extract ID from filename");
                    }
                }
            }
        }
    }

    db.flush();
    println!("Database flushed.");

    Ok(())
}
