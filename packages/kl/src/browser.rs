use anyhow::{anyhow, Result};
use kr::{Actor, Movie};
use playwright::Playwright;

pub struct BrowserScraper {
    playwright: Playwright,
}

pub struct BrowserSession {
    pub browser: playwright::api::Browser,
    pub page: playwright::api::Page,
}

impl BrowserScraper {
    pub async fn new() -> Result<Self> {
        println!("Initializing Playwright...");
        let playwright = Playwright::initialize().await?;
        println!("Preparing browsers...");
        playwright.prepare()?;
        println!("Playwright ready.");
        Ok(Self { playwright })
    }

    /// Starts a browser and waits for initial manual verification
    pub async fn start_session(&self, initial_url: &str) -> Result<BrowserSession> {
        let chromium = self.playwright.chromium();
        let browser = chromium.launcher().headless(false).launch().await?;
        let context = browser.context_builder().build().await?;
        let page = context.new_page().await?;

        println!("Opening {} for verification...", initial_url);
        page.goto_builder(initial_url).goto().await?;

        println!("**************************************************");
        println!("* BROWSER VERIFICATION                           *");
        println!("* Please pass Cloudflare / Login now.            *");
        println!("* Press ENTER here ONCE you are verified.        *");
        println!("**************************************************");

        tokio::task::spawn_blocking(|| {
            let mut input = String::new();
            let _ = std::io::stdin().read_line(&mut input);
        })
        .await?;

        Ok(BrowserSession { browser, page })
    }

    /// Automated scrape using an existing session
    pub async fn scrape_session(
        &self,
        session: &BrowserSession,
        number: &str,
        is_javdb: bool,
    ) -> Result<Movie> {
        let page = &session.page;

        if is_javdb {
            // 1. Automated Search
            let search_url = format!("https://javdb.com/search?q={}&f=all", number);
            println!("Searching for {}: {}", number, search_url);
            page.goto_builder(&search_url).goto().await?;
            self.check_verification(page).await?;

            // 2. Auto-click first result
            let clicked: bool = page
                .eval(
                    r#"() => {
                const firstLink = document.querySelector('a[href^="/v/"]');
                if (firstLink) {
                    firstLink.click();
                    return true;
                }
                return false;
            }"#,
                )
                .await?;

            if !clicked {
                return Err(anyhow!("No search results found for {}", number));
            }

            // 3. Wait for detail page
            let mut retries = 0;
            while !page.url()?.contains("/v/") && retries < 10 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                retries += 1;
            }
            self.check_verification(page).await?;

            // 4. Extraction
            self.extract_javdb(page).await
        } else {
            Err(anyhow!("Unsupported site for session scraping"))
        }
    }

    pub async fn scrape_with_interaction(&self, number: &str, is_javdb: bool) -> Result<Movie> {
        let session = self
            .start_session(if is_javdb { "https://javdb.com/" } else { "" })
            .await?;
        let movie = self.scrape_session(&session, number, is_javdb).await?;
        session.browser.close().await?;
        Ok(movie)
    }

    async fn check_verification(&self, page: &playwright::api::Page) -> Result<()> {
        let is_verification: bool = page
            .eval(
                r#"() => {
            const text = document.body.innerText;
            return text.includes('Security Verification') || 
                   text.includes('安全驗證') || 
                   text.includes('Checking your browser') ||
                   text.includes('Cloudflare') ||
                   document.title.includes('Security Verification') ||
                   document.title.includes('Cloudflare');
        }"#,
            )
            .await
            .unwrap_or(false);

        if is_verification {
            println!("**************************************************");
            println!("* SECURITY VERIFICATION DETECTED                 *");
            println!("* Please pass the challenge in the browser.      *");
            println!("* Press ENTER here ONCE you are verified.        *");
            println!("**************************************************");

            tokio::task::spawn_blocking(|| {
                let mut input = String::new();
                let _ = std::io::stdin().read_line(&mut input);
            })
            .await?;
        }
        Ok(())
    }

    async fn extract_javdb(&self, page: &playwright::api::Page) -> Result<Movie> {
        self.check_verification(page).await?;
        let current_url = page.url()?;
        println!("Extracting data from: {}", current_url);

        let title: String = page
            .eval(
                "() => {
            const el = document.querySelector('.current-title') || document.querySelector('title');
            return el ? el.innerText.split(' | JavDB')[0].trim() : 'Unknown Title';
        }",
            )
            .await?;

        let num: String = page
            .eval(
                r#"() => {
            const blocks = Array.from(document.querySelectorAll('.panel-block'));
            const el = blocks.find(e => e.innerText.includes('番號') || e.innerText.includes('ID'));
            if (!el) return '';
            const val = el.querySelector('.value');
            return val ? val.innerText.trim() : '';
        }"#,
            )
            .await?;

        let poster: Option<String> = page.eval(r#"() => {
            const img = document.querySelector('.column-video-cover img') || document.querySelector('.video-cover');
            return img ? img.src : null;
        }"#).await?;

        let releasedate: Option<String> = page.eval(r#"() => {
            const blocks = Array.from(document.querySelectorAll('.panel-block'));
            const el = blocks.find(e => e.innerText.includes('日期') || e.innerText.includes('Released'));
            if (!el) return null;
            const val = el.querySelector('.value');
            return val ? val.innerText.trim() : null;
        }"#).await?;

        let label: Option<String> = page.eval(r#"() => {
            const blocks = Array.from(document.querySelectorAll('.panel-block'));
            const el = blocks.find(e => e.innerText.includes('系列') || e.innerText.includes('Series') || e.innerText.includes('片商'));
            if (!el) return null;
            const val = el.querySelector('.value');
            return val ? val.innerText.trim() : null;
        }"#).await?;

        let actors_json: String = page.eval(r#"() => {
            const blocks = Array.from(document.querySelectorAll('.panel-block'));
            const el = blocks.find(e => e.innerText.includes('演員') || e.innerText.includes('Actor'));
            if (!el) return '[]';
            const links = Array.from(el.querySelectorAll('a'));
            return JSON.stringify(links.map(a => ({ name: a.innerText.trim() })));
        }"#).await?;

        let actors_raw: Vec<serde_json::Value> = serde_json::from_str(&actors_json)?;
        let actors = actors_raw
            .into_iter()
            .map(|v| Actor {
                name: v["name"].as_str().unwrap_or_default().to_string(),
                role: None,
                thumb: None,
            })
            .collect();

        let tags_json: String = page.eval(r#"() => {
            const blocks = Array.from(document.querySelectorAll('.panel-block'));
            const el = blocks.find(e => e.innerText.includes('類別') || e.innerText.includes('Tags') || e.innerText.includes('Genre'));
            if (!el) return '[]';
            const links = Array.from(el.querySelectorAll('a'));
            return JSON.stringify(links.map(a => a.innerText.trim()));
        }"#).await?;
        let tags: Vec<String> = serde_json::from_str(&tags_json)?;

        Ok(Movie {
            title,
            outline: None,
            poster: poster.clone(),
            thumb: poster.clone(),
            fanart: poster.clone(),
            label,
            actor: actors,
            tag: Some(tags.clone()),
            genre: Some(tags),
            num: if num.is_empty() { None } else { Some(num) },
            releasedate,
            cover: poster,
            website: Some(current_url),
        })
    }
}
