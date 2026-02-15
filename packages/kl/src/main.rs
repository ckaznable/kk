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

    /// Scrape files from WebDAV
    Webdav {
        /// Remote path to scrape
        #[arg(default_value = "/")]
        path: String,

        /// Only list files that would be scraped, without performing any action
        #[arg(short, long)]
        list_only: bool,

        /// WebDAV base URL (overrides ENV and DB)
        #[arg(long)]
        url: Option<String>,

        /// WebDAV username (overrides ENV and DB)
        #[arg(short, long)]
        user: Option<String>,

        /// WebDAV password (overrides ENV and DB)
        #[arg(short, long)]
        pass: Option<String>,

        /// JavDB cookie (overrides ENV)
        #[arg(short, long)]
        cookie: Option<String>,
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

    /// Test scrape a single ID without saving or downloading anything
    TestScrape {
        /// The ID to scrape (e.g., SSIS-123 or FC2-123456)
        id: String,

        /// JavDB cookie (overrides ENV)
        #[arg(short, long)]
        cookie: Option<String>,
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
        Commands::Webdav { url, user, pass, path, list_only, cookie } => {
            run_webdav_scraper(url, user, pass, path, list_only, cookie)?;
        }
        Commands::TestScrape { id, cookie } => {
            run_test_scrape(&id, cookie)?;
        }
    }
    Ok(())
}

fn run_test_scrape(id: &str, cookie: Option<String>) -> anyhow::Result<()> {
    println!("Testing scrape for ID: {}", id);
    let cookie = cookie.or_else(|| dirs::JAVDB_COOKIE.clone());
    let javdb = kl::javdb::JavdbScraper::with_cookie(cookie)?;
    let fc2 = kl::fc2::Fc2Scraper::new()?;

    let scraper: &dyn Scraper = if id.to_uppercase().starts_with("FC2") {
        &fc2
    } else {
        &javdb
    };

    match scraper.scrape(id) {
        Ok(movie) => {
            println!("--- Scrape Result ---");
            println!("Title:      {}", movie.title);
            println!("Number:     {}", movie.num.as_deref().unwrap_or("-"));
            println!("Release:    {}", movie.releasedate.as_deref().unwrap_or("-"));
            println!("Label:      {}", movie.label.as_deref().unwrap_or("-"));
            println!("Actors:     {}", movie.actor.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", "));
            println!("Genres:     {}", movie.genre.as_ref().map(|g| g.join(", ")).unwrap_or_default());
            println!("Tags:       {}", movie.tag.as_ref().map(|t| t.join(", ")).unwrap_or_default());
            println!("Poster URL: {}", movie.poster.as_deref().unwrap_or("-"));
            println!("Thumb URL:  {}", movie.thumb.as_deref().unwrap_or("-"));
            println!("Cover URL:  {}", movie.cover.as_deref().unwrap_or("-"));
            println!("Website:    {}", movie.website.as_deref().unwrap_or("-"));
            println!("--- End of Result ---");
        }
        Err(e) => {
            eprintln!("Scrape failed: {}", e);
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

fn run_webdav_scraper(
    url: Option<String>,
    user: Option<String>,
    pass: Option<String>,
    remote_path: String,
    list_only: bool,
    cookie: Option<String>,
) -> anyhow::Result<()> {
    let mut db = kr::db::WebDavDatabase::new()?;

    // Update DB config only if provided
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
        if let Some(u) = dirs::WEBDAV_URL.clone() {
            db.config.base_url = u;
        }
    }
    if db.config.user.is_none() {
        db.config.user = dirs::WEBDAV_USER.clone();
    }
    if db.config.pass.is_none() {
        db.config.pass = dirs::WEBDAV_PASS.clone();
    }

    if db.config.base_url.is_empty() {
        return Err(anyhow::anyhow!(
            "WebDAV URL is not set. Please provide it via --url or KK_WEBDAV_URL env var."
        ));
    }

    let client = kwa::WebDavClient::new(
        &db.config.base_url,
        db.config.user.clone().zip(db.config.pass.clone()),
    )?;

    let cookie = cookie.or_else(|| dirs::JAVDB_COOKIE.clone());

    if list_only {
        println!("Dry run mode: Listing files to be scraped from {}", remote_path);
        recursive_scan_webdav(&client, &remote_path, &mut db, None, None, &PathBuf::new(), 0, true, cookie)?;
        return Ok(());
    }

    let javdb = kl::javdb::JavdbScraper::with_cookie(cookie.clone())?;
    let fc2 = kl::fc2::Fc2Scraper::new()?;
    let cache_dir = dirs::DIR.cache_dir().join("thumbs");
    if !cache_dir.exists() {
        fs::create_dir_all(&cache_dir)?;
    }

    // Use a queue for breadth-first search or just recursion. Let's use a recursive helper.
    recursive_scan_webdav(&client, &remote_path, &mut db, Some(&javdb), Some(&fc2), &cache_dir, 0, false, cookie)?;

    db.flush();
    println!("WebDAV database flushed.");
    Ok(())
}

fn recursive_scan_webdav(
    client: &kwa::WebDavClient,
    path: &str,
    db: &mut kr::db::WebDavDatabase,
    javdb: Option<&kl::javdb::JavdbScraper>,
    fc2: Option<&kl::fc2::Fc2Scraper>,
    cache_dir: &std::path::Path,
    depth: usize,
    list_only: bool,
    cookie: Option<String>,
) -> anyhow::Result<()> {
    // Limit depth to prevent infinite loops, but 5 should be plenty for "at least two levels"
    if depth > 5 {
        return Ok(());
    }

    println!("Scanning WebDAV path: {}", path);
    let resources = match client.list(path) {
        Ok(res) => res,
        Err(e) => {
            eprintln!("  Failed to list {}: {}", path, e);
            return Ok(());
        }
    };

    for res in resources {
        // WebDAV list often includes the directory itself in the response
        if res.path == path || res.path == format!("{}/", path) || format!("{}/", res.path) == path {
            continue;
        }

        if res.is_dir {
            recursive_scan_webdav(client, &res.path, db, javdb, fc2, cache_dir, depth + 1, list_only, cookie.clone())?;
            continue;
        }

        let p = std::path::Path::new(&res.path);
        let ext = p
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_lowercase();
        if !["mp4", "mkv", "avi", "wmv", "mov", "rmvb"].contains(&ext.as_str()) {
            continue;
        }

        // Skip files smaller than 200MB
        if let Some(size) = res.size {
            if size < 200 * 1024 * 1024 {
                continue;
            }
        }

        if list_only {
            if let Some(number) = kl::number_parser::get_number(p) {
                println!("  [Found] ID: {:<12} | Path: {}", number, res.path);
            } else {
                println!("  [Skip]  No ID found  | Path: {}", res.path);
            }
            continue;
        }

        // Already in DB?
        if db.config.movies.iter().any(|m| m.url_path == res.path) {
            continue;
        }

        println!("Processing remote file: {}", res.path);
        if let Some(number) = kl::number_parser::get_number(p) {
            println!("  Found ID: {}", number);

            let scraper: &dyn Scraper = if number.to_uppercase().starts_with("FC2") {
                fc2.unwrap()
            } else {
                javdb.unwrap()
            };

            match scraper.scrape(&number) {
                Ok(mut movie) => {
                    println!("  Scraped: {}", movie.title);

                    // Download images to local cache
                    let global_thumb_path = cache_dir.join(format!("{}.jpg", number));
                    if let Some(poster_url) = &movie.poster {
                        match reqwest::blocking::get(poster_url) {
                            Ok(response) => {
                                if let Ok(bytes) = response.bytes() {
                                    if let Err(e) = fs::write(&global_thumb_path, &bytes) {
                                        eprintln!("  Failed to write to cache: {}", e);
                                    } else {
                                        println!("  Cached thumb: {:?}", global_thumb_path);
                                        movie.thumb =
                                            Some(global_thumb_path.to_string_lossy().to_string());
                                    }
                                }
                            }
                            Err(e) => eprintln!("  Failed to download poster: {}", e),
                        }
                    }

                    db.config.movies.push(kr::db::WebDavMovieData {
                        url_path: res.path,
                        movie,
                        added_time: std::time::SystemTime::now(),
                        fav: false,
                        markers: Vec::new(),
                    });
                }
                Err(e) => eprintln!("  Scrape failed: {}", e),
            }
            thread::sleep(Duration::from_secs(2));
        }
    }

    Ok(())
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
                                    Err(e) => {
                                        // Handle cross-device link error (Windows: 17, Unix: 18 usually)
                                        // or other errors that might prevent atomic rename but allow copy
                                        let raw_os_err = e.raw_os_error();
                                        if raw_os_err == Some(17) || raw_os_err == Some(18) {
                                            println!("  Cross-device link ({:?}), falling back to copy...", raw_os_err);
                                            match fs::copy(path, &video_dest) {
                                                Ok(_) => {
                                                    if let Err(del_e) = fs::remove_file(path) {
                                                        eprintln!("  Failed to delete source after copy: {}", del_e);
                                                    } else {
                                                        println!(
                                                            "  Moved video to: {:?} (via copy)",
                                                            video_dest
                                                        );
                                                    }
                                                }
                                                Err(copy_e) => {
                                                    eprintln!("  Failed to copy video: {}", copy_e)
                                                }
                                            }
                                        } else {
                                            eprintln!("  Failed to move video: {}", e);
                                        }
                                    }
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
                                // Update or Add to database
                                if let Some(idx) =
                                    db.config.movies.iter().position(|m| m.path == nfo_path)
                                {
                                    println!("  Updating existing DB entry for: {:?}", nfo_path);
                                    db.config.movies[idx].movie = movie;
                                } else {
                                    let movie_data = kr::db::MovieData {
                                        path: nfo_path,
                                        movie,
                                        added_time: std::time::SystemTime::now(),
                                        fav: false,
                                        markers: Vec::new(),
                                    };
                                    db.config.movies.push(movie_data);
                                }
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
