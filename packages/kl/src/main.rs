use clap::{Parser, Subcommand};
use kl::Scraper;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
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

        /// Use legacy headless HTTP scraper (no browser)
        #[arg(long)]
        headless: bool,
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

        /// Use legacy headless HTTP scraper (no browser)
        #[arg(long)]
        headless: bool,
    },

    /// Test scrape a single ID without saving or downloading anything
    TestScrape {
        /// The ID to scrape (e.g., SSIS-123 or FC2-123456)
        id: String,

        /// JavDB cookie (overrides ENV)
        #[arg(short, long)]
        cookie: Option<String>,

        /// Use legacy headless HTTP scraper (no browser)
        #[arg(long)]
        headless: bool,
    },

    /// Start the cache HTTP server that accepts pre-fetched HTML and stores parsed metadata
    Serve {
        /// Port to listen on (default: 6969)
        #[arg(short, long, default_value = "6969")]
        port: u16,
    },

    /// Pull ready WebDAV downloads from ks, tidy locally, then delete ks cache
    PullKs {
        /// ks server base URL (overrides config.toml)
        #[arg(long)]
        url: Option<String>,

        /// Destination media root (defaults to KK_SEARCH_PATH)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Only list ks-ready files without downloading/tidying
        #[arg(long)]
        dry_run: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::FixDb {
            dry_run,
            test_first,
            fix_num,
            list_need_fix,
            headless,
        } => {
            run_fix_db(dry_run, test_first, fix_num, list_need_fix, headless).await?;
        }
        Commands::Tidy { input, output } => {
            let output_path = output.unwrap_or_else(|| dirs::SEARCH_PATH.to_path_buf());
            run_scraper(input, output_path).await?;
        }
        Commands::Webdav {
            url,
            user,
            pass,
            path,
            list_only,
            cookie,
            headless,
        } => {
            run_webdav_scraper(url, user, pass, path, list_only, cookie, headless).await?;
        }
        Commands::TestScrape {
            id,
            cookie,
            headless,
        } => {
            run_test_scrape(&id, cookie, headless).await?;
        }
        Commands::Serve { port } => {
            kl::server::run_server(port).await?;
        }
        Commands::PullKs {
            url,
            output,
            dry_run,
        } => {
            let output_path = output.unwrap_or_else(|| dirs::SEARCH_PATH.to_path_buf());
            run_pull_ks_downloads(url, output_path, dry_run).await?;
        }
    }
    Ok(())
}

fn run_test_scrape(
    id: &str,
    cookie: Option<String>,
    headless: bool,
) -> impl std::future::Future<Output = anyhow::Result<()>> {
    let id = id.to_string();
    async move {
        println!("Testing scrape for ID: {}", id);

        let movie = if !headless {
            let scraper = kl::browser::BrowserScraper::new().await?;
            let is_fc2 = id.to_uppercase().starts_with("FC2");
            if is_fc2 {
                return Err(anyhow::anyhow!(
                    "Browser scraping not yet implemented for FC2"
                ));
            }

            scraper.scrape_with_interaction(&id, true).await?
        } else {
            let cookie = cookie.or_else(|| dirs::JAVDB_COOKIE.clone());
            let javdb = kl::javdb::JavdbScraper::with_cookie(cookie)?;
            let fc2 = kl::fc2::Fc2Scraper::new()?;

            let scraper: &dyn Scraper = if id.to_uppercase().starts_with("FC2") {
                &fc2
            } else {
                &javdb
            };
            scraper.scrape(&id).await?
        };

        println!("--- Scrape Result ---");
        println!("Title:      {}", movie.title);
        println!("Number:     {}", movie.num.as_deref().unwrap_or("-"));
        println!(
            "Release:    {}",
            movie.releasedate.as_deref().unwrap_or("-")
        );
        println!("Label:      {}", movie.label.as_deref().unwrap_or("-"));
        println!(
            "Actors:     {}",
            movie
                .actor
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        println!(
            "Genres:     {}",
            movie
                .genre
                .as_ref()
                .map(|g| g.join(", "))
                .unwrap_or_default()
        );
        println!(
            "Tags:       {}",
            movie.tag.as_ref().map(|t| t.join(", ")).unwrap_or_default()
        );
        println!("Poster URL: {}", movie.poster.as_deref().unwrap_or("-"));
        println!("Thumb URL:  {}", movie.thumb.as_deref().unwrap_or("-"));
        println!("Cover URL:  {}", movie.cover.as_deref().unwrap_or("-"));
        println!("Website:    {}", movie.website.as_deref().unwrap_or("-"));
        println!("--- End of Result ---");

        Ok(())
    }
}

async fn run_fix_db(
    dry_run: bool,
    test_first: bool,
    fix_num: Option<usize>,
    list_need_fix: bool,
    headless: bool,
) -> anyhow::Result<()> {
    // Explicitly load DB configuration instead of using default empty init
    let mut db = kr::db::SimpleJsonDatabase::new()
        .map_err(|e| anyhow::anyhow!("Failed to load database: {}", e))?;
    let javdb = kl::javdb::JavdbScraper::new()?;
    let fc2 = kl::fc2::Fc2Scraper::new()?;

    // Initialize browser session if requested (default)
    let mut browser_session = None;
    let browser_scraper = if !headless {
        let scraper = kl::browser::BrowserScraper::new().await?;
        browser_session = Some(scraper.start_session("https://javdb.com/").await?);
        Some(scraper)
    } else {
        None
    };

    let mut modified = false;
    let mut fix_count = 0;
    let mut found_issues_count = 0;

    let len = db.config.movies.len();
    println!(
        "Database path: {:?}",
        dirs::DIR.config_local_dir().join("kr.json")
    );
    println!("Loaded {} movies from database.", len);

    let cache_dir = &*dirs::THUMB_CACHE_DIR;
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
            let p = db.config.movies[i].abs_path();
            file_stem_for_id = p.file_stem().unwrap().to_string_lossy().to_string();
        }

        // Fix: if num is purely alphabetic (only letters, no "-"), replace with file stem
        if let Some(ref num) = movie_num_opt {
            let is_alpha_only = !num.is_empty()
                && num.chars().all(|c| c.is_ascii_alphabetic())
                && !num.contains('-');
            if is_alpha_only {
                println!(
                    "  [Fix Num] '{}' is alpha-only, updating num to file stem '{}'",
                    num, file_stem_for_id
                );
                if !dry_run {
                    db.config.movies[i].movie.num = Some(file_stem_for_id.clone());
                    modified = true;
                }
            }
        }

        // Try getting ID from movie.num, fallback to file_stem
        let movie_num = db.config.movies[i].movie.num.clone();
        let cache_id = movie_num
            .clone()
            .unwrap_or_else(|| file_stem_for_id.clone());
        let thumb_filename = format!("{}.jpg", cache_id); // Using .jpg by convention
        let cache_thumb_path = cache_dir.join(&thumb_filename);
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

        let nfo_path = db.config.movies[i].abs_path();
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

                match download_file(&url, &dest).await {
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
                        db.config.movies[i].movie.thumb = Some(thumb_filename.clone());

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

                let scrape_result = if let Some(session) = &browser_session {
                    browser_scraper
                        .as_ref()
                        .unwrap()
                        .scrape_session(session, &id, !id.to_uppercase().starts_with("FC2"))
                        .await
                } else {
                    let scraper: &dyn Scraper = if id.to_uppercase().starts_with("FC2") {
                        &fc2
                    } else {
                        &javdb
                    };
                    scraper.scrape(&id).await
                };

                match scrape_result {
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
                            if let Ok(_) = download_file(poster_url, &target_img_path).await {
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
                                    new_movie.thumb = Some(thumb_filename.clone());
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

                thread::sleep(Duration::from_secs(6));
            }
        }
    }

    if list_need_fix {
        println!("Total items needing fix: {}", found_issues_count);
        return Ok(());
    }

    if let Some(session) = browser_session {
        session.browser.close().await?;
    }

    if modified {
        db.flush();
        println!("Database flushed with {} fixes.", fix_count);

        // Push to ks server if configured
        if let Some(ref url) = dirs::ks_base_url() {
            println!("[fix-db] Syncing kr.json to ks...");
            if let Err(e) = kr::sync::push_kr(url) {
                eprintln!("[fix-db] Failed to push kr.json to ks: {}", e);
            }
        }
    } else {
        println!("No changes made.");
    }

    Ok(())
}

async fn download_file(url: &str, path: &PathBuf) -> anyhow::Result<()> {
    let bytes = reqwest::get(url).await?.bytes().await?;
    let mut file = fs::File::create(path)?;
    file.write_all(&bytes)?;
    Ok(())
}

fn update_nfo_file(movie_data: &kr::db::MovieData) {
    if let Ok(xml) = kl::generate_nfo_xml(&movie_data.movie) {
        let _ = fs::write(movie_data.abs_path(), xml);
    }
}

async fn run_webdav_scraper(
    url: Option<String>,
    user: Option<String>,
    pass: Option<String>,
    remote_path: String,
    list_only: bool,
    cookie: Option<String>,
    headless: bool,
) -> anyhow::Result<()> {
    let mut db = kr::db::WebDavDatabase::new()?;

    // ... (config logic)
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

    let local_db = kr::db::SimpleJsonDatabase::new().ok();

    let client = kwa::WebDavClient::new(
        &db.config.base_url,
        db.config.user.clone().zip(db.config.pass.clone()),
    )?;

    let cookie = cookie.or_else(|| dirs::JAVDB_COOKIE.clone());

    if list_only {
        println!(
            "Dry run mode: Listing files to be scraped from {}",
            remote_path
        );
        recursive_scan_webdav(
            &client,
            &remote_path,
            &mut db,
            local_db.as_ref(),
            None,
            None,
            &PathBuf::new(),
            0,
            true,
            cookie,
            None,
        )
        .await?;
        return Ok(());
    }

    let javdb = kl::javdb::JavdbScraper::with_cookie(cookie.clone())?;
    let fc2 = kl::fc2::Fc2Scraper::new()?;
    let cache_dir = &*dirs::THUMB_CACHE_DIR;

    // Initialize browser session if requested (default)
    let mut browser_session = None;
    let browser_scraper = if !headless {
        let scraper = kl::browser::BrowserScraper::new().await?;
        browser_session = Some(scraper.start_session("https://javdb.com/").await?);
        Some(scraper)
    } else {
        None
    };

    // Use a queue for breadth-first search or just recursion. Let's use a recursive helper.
    recursive_scan_webdav(
        &client,
        &remote_path,
        &mut db,
        local_db.as_ref(),
        Some(&javdb),
        Some(&fc2),
        &cache_dir,
        0,
        false,
        cookie,
        browser_scraper.as_ref().zip(browser_session.as_ref()),
    )
    .await?;

    if let Some(session) = browser_session {
        session.browser.close().await?;
    }

    db.flush();
    println!("WebDAV database flushed.");
    Ok(())
}

async fn run_pull_ks_downloads(
    url: Option<String>,
    output: PathBuf,
    dry_run: bool,
) -> anyhow::Result<()> {
    let base_url = url
        .or_else(dirs::ks_base_url)
        .ok_or_else(|| anyhow::anyhow!("No ks base URL provided. Pass --url or set ks.base_url"))?;
    let base_url = base_url.trim_end_matches('/').to_string();

    println!(
        "[pull-ks] Start workflow: base_url={}, output={:?}, dry_run={}",
        base_url, output, dry_run
    );

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .build()?;
    let list_url = format!("{}/downloads/webdav/ready", base_url);
    println!("[pull-ks] Request ready list: GET {}", list_url);
    let list_resp = client.get(&list_url).send().await?;
    println!(
        "[pull-ks] Ready list response status: {}",
        list_resp.status()
    );
    if !list_resp.status().is_success() {
        return Err(anyhow::anyhow!(
            "ks returned {} for GET /downloads/webdav/ready",
            list_resp.status()
        ));
    }

    let ready: Vec<kr::sync::KsReadyDownload> = list_resp.json().await?;
    if ready.is_empty() {
        println!("[pull-ks] No ready downloads on ks.");
        return Ok(());
    }

    println!("[pull-ks] Found {} ready download(s) on ks.", ready.len());
    for item in &ready {
        println!(
            "[pull-ks] Ready item: id={}, file={}, size={} bytes, path={}",
            item.id, item.file_name, item.size_bytes, item.url_path
        );
    }

    if dry_run {
        for item in ready {
            println!(
                "  [{}] {} ({} bytes) <- {}",
                item.id, item.file_name, item.size_bytes, item.url_path
            );
        }
        println!("[pull-ks] Dry run enabled, stopping before download.");
        return Ok(());
    }

    let staging_dir = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("kk_ks_ready_staging");
    if !staging_dir.exists() {
        println!("[pull-ks] Creating staging dir: {:?}", staging_dir);
        fs::create_dir_all(&staging_dir)?;
    } else {
        println!("[pull-ks] Using existing staging dir: {:?}", staging_dir);
    }

    let mut total_success = 0usize;
    let mut total_failed = 0usize;
    let total_ready = ready.len();

    'item: for (idx, item) in ready.into_iter().enumerate() {
        println!(
            "[pull-ks] [{}/{}] Processing: id={}, file={}, size={} bytes",
            idx + 1,
            total_ready,
            item.id,
            item.file_name,
            item.size_bytes
        );

        // ── Step 1: Download ──────────────────────────────────────────────
        let file_url = format!("{}/downloads/webdav/ready/{}/file", base_url, item.id);
        println!("[pull-ks] Download request: GET {}", file_url);
        let mut resp = client.get(&file_url).send().await?;
        if !resp.status().is_success() {
            eprintln!(
                "[pull-ks] Failed to download {} from ks: HTTP {}",
                item.file_name,
                resp.status()
            );
            total_failed += 1;
            continue;
        }

        let stage_path = unique_stage_path(&staging_dir, &item.file_name);
        let mut file = fs::File::create(&stage_path)?;
        let mut downloaded_bytes = 0u64;
        let expected_size = if item.size_bytes > 0 {
            item.size_bytes
        } else {
            resp.content_length().unwrap_or(0)
        };
        let mut next_progress_pct = 10u64;
        let mut idle_timeout_count = 0u32;

        loop {
            let chunk_result = tokio::time::timeout(Duration::from_secs(20), resp.chunk()).await;
            match chunk_result {
                Ok(Ok(Some(chunk))) => {
                    idle_timeout_count = 0;
                    file.write_all(&chunk)?;
                    downloaded_bytes += chunk.len() as u64;

                    if expected_size > 0 {
                        let pct =
                            (downloaded_bytes.saturating_mul(100) / expected_size).min(100);
                        while pct >= next_progress_pct {
                            println!(
                                "[pull-ks] [{} / {}] {} progress: {}% ({}/{})",
                                idx + 1,
                                total_ready,
                                item.file_name,
                                next_progress_pct,
                                downloaded_bytes,
                                expected_size
                            );
                            next_progress_pct += 10;
                        }
                    }
                }
                Ok(Ok(None)) => break,
                Ok(Err(e)) => {
                    eprintln!("[pull-ks] Download error for {}: {}", item.file_name, e);
                    total_failed += 1;
                    // Remove partial file
                    let _ = fs::remove_file(&stage_path);
                    continue 'item;
                }
                Err(_) => {
                    idle_timeout_count += 1;
                    eprintln!(
                        "[pull-ks] [{} / {}] waiting for data: {} (no chunks for {}s, downloaded={} bytes)",
                        idx + 1,
                        total_ready,
                        item.file_name,
                        idle_timeout_count * 20,
                        downloaded_bytes
                    );
                    if idle_timeout_count >= 30 {
                        eprintln!(
                            "[pull-ks] Download stalled for {}: giving up after {} seconds",
                            item.file_name,
                            idle_timeout_count * 20
                        );
                        total_failed += 1;
                        let _ = fs::remove_file(&stage_path);
                        continue 'item;
                    }
                }
            }
        }

        if expected_size > 0 && next_progress_pct <= 100 {
            println!(
                "[pull-ks] [{} / {}] {} progress: 100% ({}/{})",
                idx + 1,
                total_ready,
                item.file_name,
                downloaded_bytes,
                expected_size
            );
        }
        println!(
            "[pull-ks] Downloaded: {} -> {:?} ({} bytes)",
            item.file_name, stage_path, downloaded_bytes
        );

        // ── Step 2: Tidy ─────────────────────────────────────────────────
        println!(
            "[pull-ks] [{}/{}] Tidy: {:?} -> {:?}",
            idx + 1,
            total_ready,
            staging_dir,
            output
        );
        if let Err(e) = run_scraper(staging_dir.clone(), output.clone()).await {
            eprintln!("[pull-ks] Tidy failed for {}: {}", item.file_name, e);
            total_failed += 1;
            // Leave the file for manual inspection
            continue;
        }
        println!("[pull-ks] [{}/{}] Tidy completed.", idx + 1, total_ready);

        // ── Step 3: Delete ks cache ───────────────────────────────────────
        // Only delete from ks if tidy moved the file (staging file is gone)
        if stage_path.exists() {
            eprintln!(
                "[pull-ks] Skip ks delete for {}: staging file still exists after tidy ({:?})",
                item.id, stage_path
            );
            total_failed += 1;
            continue;
        }

        let delete_url = format!("{}/downloads/webdav/ready/{}", base_url, item.id);
        println!("[pull-ks] Delete request: DELETE {}", delete_url);
        let del_resp = client.delete(&delete_url).send().await?;
        if del_resp.status().is_success() {
            total_success += 1;
            println!(
                "[pull-ks] [{}/{}] Done: deleted ks ready item {}",
                idx + 1,
                total_ready,
                item.id
            );
        } else {
            eprintln!(
                "[pull-ks] Failed to delete ks ready item {}: HTTP {}",
                item.id,
                del_resp.status()
            );
            total_failed += 1;
        }
    }

    println!(
        "[pull-ks] Completed workflow: success={}, failed={}",
        total_success, total_failed
    );
    Ok(())
}

fn unique_stage_path(dir: &Path, file_name: &str) -> PathBuf {
    let mut candidate = dir.join(file_name);
    if !candidate.exists() {
        return candidate;
    }

    let stem = Path::new(file_name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "download".to_string());
    let ext = Path::new(file_name)
        .extension()
        .map(|s| s.to_string_lossy().to_string());

    let mut n = 1usize;
    loop {
        let alt_name = if let Some(ref e) = ext {
            format!("{}-{}.{}", stem, n, e)
        } else {
            format!("{}-{}", stem, n)
        };
        candidate = dir.join(alt_name);
        if !candidate.exists() {
            return candidate;
        }
        n += 1;
    }
}

#[async_recursion::async_recursion]
async fn recursive_scan_webdav(
    client: &kwa::WebDavClient,
    path: &str,
    db: &mut kr::db::WebDavDatabase,
    local_db: Option<&kr::db::SimpleJsonDatabase>,
    javdb: Option<&kl::javdb::JavdbScraper>,
    fc2: Option<&kl::fc2::Fc2Scraper>,
    cache_dir: &std::path::Path,
    depth: usize,
    list_only: bool,
    cookie: Option<String>,
    browser: Option<(&kl::browser::BrowserScraper, &kl::browser::BrowserSession)>,
) -> anyhow::Result<()> {
    // Limit depth to prevent infinite loops, but 5 should be plenty for "at least two levels"
    if depth > 5 {
        return Ok(());
    }

    println!("Scanning WebDAV path: {}", path);
    let resources = match client.list(path).await {
        Ok(res) => res,
        Err(e) => {
            eprintln!("  Failed to list {}: {}", path, e);
            return Ok(());
        }
    };

    for res in resources {
        // WebDAV list often includes the directory itself in the response
        if res.path == path || res.path == format!("{}/", path) || format!("{}/", res.path) == path
        {
            continue;
        }

        if res.is_dir {
            recursive_scan_webdav(
                client,
                &res.path,
                db,
                local_db,
                javdb,
                fc2,
                cache_dir,
                depth + 1,
                list_only,
                cookie.clone(),
                browser,
            )
            .await?;
            continue;
        }

        // Normalize path: some servers return full URLs in href, we want the path relative to base or absolute path
        let mut res_path = res.path.clone();
        if let Ok(url) = reqwest::Url::parse(&res_path) {
            res_path = url.path().to_string();
        }

        let p = std::path::Path::new(&res_path);
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

        // Check if ID is already in DB
        let number = kl::number_parser::get_number(p);
        if let Some(ref num) = number {
            if db.contains_id(num) {
                continue;
            }
        }

        // Already in DB by path?
        if db.config.movies.iter().any(|m| m.url_path == res_path) {
            continue;
        }

        if list_only {
            if let Some(ref num) = number {
                println!("  [Found] ID: {:<12} | Path: {}", num, res_path);
            } else {
                println!("  [Skip]  No ID found  | Path: {}", res_path);
            }
            continue;
        }

        println!("Processing remote file: {}", res_path);
        if let Some(num) = number {
            println!("  Found ID: {}", num);

            let mut existing_movie = None;

            // 1. Check kk_cache first
            {
                let cache = kl::server::KkCache::load();
                if let Some(m) = cache.find(&num) {
                    println!("  Found in kk_cache, reusing metadata.");
                    existing_movie = Some(m.clone());
                }
            }

            // 2. Fall back to local DB
            if existing_movie.is_none() {
                if let Some(local) = local_db {
                    if let Some(m) = local.find_movie_by_id(&num) {
                        println!("  Found in local database, reusing metadata.");
                        existing_movie = Some(m.clone());
                    }
                }
            }

            let mut did_scrape = false;
            let scrape_result = if let Some(m) = existing_movie {
                Ok(m)
            } else if let Some((scraper, session)) = browser {
                did_scrape = true;
                scraper
                    .scrape_session(session, &num, !num.to_uppercase().starts_with("FC2"))
                    .await
            } else {
                did_scrape = true;
                let scraper: &dyn Scraper = if num.to_uppercase().starts_with("FC2") {
                    fc2.unwrap()
                } else {
                    javdb.unwrap()
                };
                scraper.scrape(&num).await
            };

            match scrape_result {
                Ok(mut movie) => {
                    println!("  Scraped: {}", movie.title);

                    let thumb_filename = format!("{}.jpg", num);
                    let global_thumb_path = cache_dir.join(&thumb_filename);
                    if !global_thumb_path.exists() {
                        if let Some(poster_url) = &movie.poster {
                            if poster_url.starts_with("http") {
                                match reqwest::get(poster_url).await {
                                    Ok(response) => {
                                        if let Ok(bytes) = response.bytes().await {
                                            if let Err(e) = fs::write(&global_thumb_path, &bytes) {
                                                eprintln!("  Failed to write to cache: {}", e);
                                            } else {
                                                println!("  Cached thumb: {:?}", global_thumb_path);
                                            }
                                        }
                                    }
                                    Err(e) => eprintln!("  Failed to download poster: {}", e),
                                }
                            }
                        }
                    }

                    if global_thumb_path.exists() {
                        movie.thumb = Some(thumb_filename);
                    }

                    db.config.movies.push(kr::db::WebDavMovieData {
                        url_path: res_path,
                        file_size: res.size,
                        movie,
                        added_time: std::time::SystemTime::now(),
                        fav: false,
                        markers: Vec::new(),
                        pending_download: false,
                    });

                    // Flush after each successful scrape to avoid losing progress
                    db.flush();
                }
                Err(e) => eprintln!("  Scrape failed: {}", e),
            }
            if did_scrape {
                thread::sleep(Duration::from_secs(6));
            }
        }
    }

    Ok(())
}

async fn run_scraper(input: PathBuf, output: PathBuf) -> anyhow::Result<()> {
    if !output.exists() {
        fs::create_dir_all(&output)?;
    }

    let cache_dir = &*dirs::THUMB_CACHE_DIR;

    // Load database
    let mut db = kr::db::SimpleJsonDatabase::new()
        .map_err(|e| anyhow::anyhow!("Failed to load database: {}", e))?;

    let dav_db = kr::db::WebDavDatabase::new().ok();

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

                        let mut existing_movie = None;

                        // 1. Check kk_cache first
                        {
                            let cache = kl::server::KkCache::load();
                            if let Some(m) = cache.find(&number) {
                                println!("  Found in kk_cache, reusing metadata.");
                                existing_movie = Some(m.clone());
                            }
                        }

                        // 2. Fall back to WebDAV DB
                        if existing_movie.is_none() {
                            if let Some(dav) = &dav_db {
                                if let Some(m) = dav.find_movie_by_id(&number) {
                                    println!("  Found in WebDAV database, reusing metadata.");
                                    existing_movie = Some(m.clone());
                                }
                            }
                        }

                        let filename_starts_fc2 = path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_lowercase().starts_with("fc2"))
                            .unwrap_or(false);

                        let mut did_scrape = false;
                        let scrape_result = if let Some(m) = existing_movie {
                            Ok(m)
                        } else {
                            did_scrape = true;
                            let scraper: &dyn Scraper = if number.to_uppercase().starts_with("FC2")
                                || filename_starts_fc2
                            {
                                &fc2
                            } else {
                                &javdb
                            };
                            scraper.scrape(&number).await
                        };

                        match scrape_result {
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
                                match move_file(path, &video_dest) {
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
                                let thumb_filename = format!("{}.jpg", number);
                                let global_thumb_path = cache_dir.join(&thumb_filename);

                                if let Some(poster_url) = &movie.poster {
                                    // Download poster to movie dir
                                    let img_path = movie_dir.join(format!("{}-poster.jpg", number));

                                    let mut downloaded = false;
                                    if global_thumb_path.exists() && !img_path.exists() {
                                        if let Ok(_) = fs::copy(&global_thumb_path, &img_path) {
                                            println!("  Copied Poster from cache: {:?}", img_path);
                                            downloaded = true;
                                        }
                                    }

                                    if !downloaded && poster_url.starts_with("http") {
                                        match reqwest::get(poster_url).await {
                                            Ok(response) => {
                                                if let Ok(bytes) = response.bytes().await {
                                                    if let Ok(mut file) =
                                                        fs::File::create(&img_path)
                                                    {
                                                        file.write_all(&bytes).ok();
                                                        println!("  Saved Poster: {:?}", img_path);
                                                        downloaded = true;

                                                        // Copy to global cache if not there
                                                        if !global_thumb_path.exists() {
                                                            if let Err(e) = fs::write(
                                                                &global_thumb_path,
                                                                &bytes,
                                                            ) {
                                                                eprintln!("  Failed to write to cache: {}", e);
                                                            } else {
                                                                println!(
                                                                    "  Cached thumb: {:?}",
                                                                    global_thumb_path
                                                                );
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                eprintln!("  Failed to download poster: {}", e)
                                            }
                                        }
                                    }

                                    if downloaded || global_thumb_path.exists() {
                                        movie.thumb = Some(thumb_filename);
                                        movie.poster = Some(img_path.to_string_lossy().to_string());
                                    }
                                }

                                // Add to database
                                // Update or Add to database
                                let relative_nfo_path = nfo_path
                                    .strip_prefix(&*dirs::SEARCH_PATH)
                                    .unwrap_or(&nfo_path)
                                    .to_owned();
                                if let Some(idx) = db
                                    .config
                                    .movies
                                    .iter()
                                    .position(|m| m.path == relative_nfo_path)
                                {
                                    println!(
                                        "  Updating existing DB entry for: {:?}",
                                        relative_nfo_path
                                    );
                                    db.config.movies[idx].movie = movie;
                                } else {
                                    let movie_data = kr::db::MovieData {
                                        path: relative_nfo_path,
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

                        // Add delay to avoid rate limiting if we actually scraped
                        if did_scrape {
                            thread::sleep(Duration::from_secs(6));
                        }
                    } else {
                        println!("  Could not extract ID from filename");
                    }
                }
            }
        }
    }

    db.flush();
    println!("Database flushed.");

    // Push to ks server if configured
    if let Some(ref url) = dirs::ks_base_url() {
        println!("[tidy] Syncing kr.json to ks...");
        if let Err(e) = kr::sync::push_kr(url) {
            eprintln!("[tidy] Failed to push kr.json to ks: {}", e);
        }
    }

    Ok(())
}

/// Move a file from `src` to `dst`, falling back to copy+delete when a
/// cross-device rename is not possible (e.g. src and dst are on different
/// filesystems / mount points).
fn move_file(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices
            || e.raw_os_error() == Some(18) // EXDEV on Linux
            || e.raw_os_error() == Some(17) // EXDEV on some other Unix/Windows
        => {
            // Atomic rename is not possible across devices; fall back to copy + delete.
            fs::copy(src, dst)?;
            fs::remove_file(src)?;
            Ok(())
        }
        Err(e) => Err(e),
    }
}
