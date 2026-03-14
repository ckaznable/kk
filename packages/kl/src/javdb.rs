use anyhow::{anyhow, Result};
use kr::{Actor, Movie};
use reqwest::Client;
use scraper::{Html, Selector};

pub struct JavdbScraper {
    client: Client,
    site: String,
    cookie: Option<String>,
}

impl JavdbScraper {
    pub fn new() -> Result<Self> {
        Self::with_cookie(None)
    }

    pub fn with_cookie(cookie: Option<String>) -> Result<Self> {
        let mut headers = reqwest::header::HeaderMap::new();
        // ... (headers initialization remains same)
        headers.insert(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/144.0.0.0 Safari/537.36".parse()?,
        );
        // ... (other headers)
        headers.insert(
            "Accept-Language",
            "zh-TW,zh;q=0.9,en-US;q=0.8,en;q=0.7".parse()?,
        );
        headers.insert("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7".parse()?);
        headers.insert("Cache-Control", "max-age=0".parse()?);
        headers.insert(
            "Sec-Ch-Ua",
            "\"Not(A:Brand\";v=\"99\", \"Google Chrome\";v=\"144\", \"Chromium\";v=\"144\""
                .parse()?,
        );
        headers.insert("Sec-Ch-Ua-Mobile", "?0".parse()?);
        headers.insert("Sec-Ch-Ua-Platform", "\"Windows\"".parse()?);

        let client = Client::builder()
            .cookie_store(true)
            .default_headers(headers)
            .build()?;

        Ok(Self {
            client,
            site: "javdb".to_string(),
            cookie,
        })
    }

    pub fn with_site(site: String) -> Result<Self> {
        let mut scraper = Self::new()?;
        scraper.site = site;
        Ok(scraper)
    }

    pub async fn search_movie(&self, number: &str) -> Result<String> {
        let clean_number = number.replace("-", "").replace("_", "").to_uppercase();
        let search_url = format!("https://{}.com/search?q={}&f=all", self.site, number);

        let mut request = self.client.get(&search_url);

        let mut cookie_str = "over18=1; theme=auto; locale=zh".to_string();
        if let Some(ref c) = self.cookie {
            cookie_str = format!("{}; {}", cookie_str, c);
        }
        request = request.header("Cookie", cookie_str);

        let response = request.send().await?;

        if response.status() == reqwest::StatusCode::FORBIDDEN {
            eprintln!("Error: 403 Forbidden when searching JavDB. This usually means Cloudflare protection is active.");
            return Err(anyhow!(
                "JavDB access forbidden (403). Cloudflare challenge active."
            ));
        }

        let html = response.text().await?;
        let document = Html::parse_document(&html);

        // Broadest possible selectors
        let item_selector =
            Selector::parse(".movie-list > div, .item, .column, .box, .grid-item").unwrap();
        let url_selector = Selector::parse("a").unwrap();

        let mut results = Vec::new();

        for item in document.select(&item_selector) {
            let a_tags = item.select(&url_selector).collect::<Vec<_>>();
            for a in a_tags {
                if let Some(href) = a.value().attr("href") {
                    if href.starts_with("/v/") {
                        let text = item.text().collect::<String>().to_uppercase();
                        results.push((href.to_string(), text));
                    }
                }
            }
        }

        // If no results from structured search, try finding ANY link starting with /v/
        if results.is_empty() {
            let all_links = Selector::parse("a[href^='/v/']").unwrap();
            for a in document.select(&all_links) {
                let href = a.value().attr("href").unwrap();
                let text = a.text().collect::<String>().to_uppercase();
                let title_attr = a.value().attr("title").unwrap_or_default().to_uppercase();
                results.push((href.to_string(), format!("{} {}", text, title_attr)));
            }
        }

        // Find matching URL
        let correct_url = if let Some((url, _)) = results.iter().find(|(_, t)| {
            let t_clean = t.replace("-", "").replace("_", "");
            t_clean.contains(&clean_number) || clean_number.contains(&t_clean)
        }) {
            url
        } else if !results.is_empty() {
            // Last resort: if the results list is small, or the search was specific, pick the first one
            &results[0].0
        } else {
            return Err(anyhow!("Movie not found in javdb (no results): {}", number));
        };

        Ok(format!("https://{}.com{}", self.site, correct_url))
    }

    pub async fn scrape(&self, number: &str) -> Result<Movie> {
        let detail_url = self.search_movie(number).await?;

        let mut request = self.client.get(&detail_url);

        let mut cookie_str = "over18=1; theme=auto; locale=zh".to_string();
        if let Some(ref c) = self.cookie {
            cookie_str = format!("{}; {}", cookie_str, c);
        }
        request = request.header("Cookie", cookie_str);

        let response = request.send().await?;

        if response.status() == reqwest::StatusCode::FORBIDDEN {
            eprintln!("Error: 403 Forbidden when accessing JavDB details. This usually means Cloudflare protection is active.");
            return Err(anyhow!("JavDB details access forbidden (403)."));
        }

        let html = response.text().await?;

        // Check if authentication is required
        if html.contains("此內容需要登入才能查看或操作")
            || html.contains("需要VIP權限才能訪問此內容")
        {
            return Err(anyhow!("Authentication required for: {}", number));
        }

        let document = Html::parse_document(&html);

        // Extract data
        let title = self.extract_title(&document, number)?;
        let outline = self.extract_outline(&document);
        let poster = self.extract_cover(&document);
        let cover = poster.clone();
        let actors = self.extract_actors(&document);
        let tags = self.extract_tags(&document);
        let genres = self.extract_genres(&document);
        let num = self.extract_number(&document, number)?;
        let releasedate = self.extract_release_date(&document);
        let label = self.extract_label(&document);

        Ok(Movie {
            title,
            outline,
            poster: poster.clone(),
            thumb: poster.clone(),
            fanart: poster.clone(),
            label,
            actor: actors,
            tag: tags,
            genre: genres,
            num: Some(num),
            releasedate,
            cover,
            website: Some(detail_url),
        })
    }

    /// Parse movie metadata from pre-fetched HTML string (used by cache server).
    /// `number` is used as fallback for title/num extraction.
    pub fn parse_html(&self, number: &str, html: &str) -> Result<Movie> {
        // Check if authentication is required
        if html.contains("此內容需要登入才能查看或操作")
            || html.contains("需要VIP權限才能訪問此內容")
        {
            return Err(anyhow!("Authentication required for: {}", number));
        }

        let document = Html::parse_document(html);

        let title = self.extract_title(&document, number)?;
        let outline = self.extract_outline(&document);
        let poster = self.extract_cover(&document);
        let cover = poster.clone();
        let actors = self.extract_actors(&document);
        let tags = self.extract_tags(&document);
        let genres = self.extract_genres(&document);
        let num = self.extract_number(&document, number)?;
        let releasedate = self.extract_release_date(&document);
        let label = self.extract_label(&document);

        Ok(Movie {
            title,
            outline,
            poster: poster.clone(),
            thumb: poster.clone(),
            fanart: poster.clone(),
            label,
            actor: actors,
            tag: tags,
            genre: genres,
            num: Some(num),
            releasedate,
            cover,
            website: None,
        })
    }

    fn extract_title(&self, document: &Html, number: &str) -> Result<String> {
        let title_selector = Selector::parse("title").unwrap();

        if let Some(title_el) = document.select(&title_selector).next() {
            let title_text = title_el.text().collect::<String>();
            // Remove " | JavDB" and the number from title
            let title = title_text
                .split(" | JavDB")
                .next()
                .unwrap_or(&title_text)
                .trim()
                .replace(number, "")
                .trim()
                .to_string();
            Ok(title)
        } else {
            let current_title_selector = Selector::parse(".current-title").unwrap();
            if let Some(title_el) = document.select(&current_title_selector).next() {
                let title_text = title_el.text().collect::<String>();
                let title = title_text.trim().replace(number, "").trim().to_string();
                Ok(title)
            } else {
                Err(anyhow!("Title not found"))
            }
        }
    }

    fn extract_outline(&self, _document: &Html) -> Option<String> {
        // JavDB doesn't usually have outline/description in the main page
        None
    }

    fn extract_cover(&self, document: &Html) -> Option<String> {
        let cover_selector =
            Selector::parse(".column-video-cover a img, .column-video-cover img").unwrap();

        document
            .select(&cover_selector)
            .next()
            .and_then(|el| el.value().attr("src"))
            .map(|s| s.to_string())
    }

    fn extract_actors(&self, document: &Html) -> Vec<Actor> {
        // Since CSS :contains is not supported in scraper, we need a different approach
        let panel_selector = Selector::parse(".panel-block").unwrap();
        let strong_selector = Selector::parse("strong").unwrap();
        let value_selector = Selector::parse(".value").unwrap();
        let actor_link_selector = Selector::parse("a[href*='/actors/']").unwrap();

        for panel in document.select(&panel_selector) {
            if let Some(strong) = panel.select(&strong_selector).next() {
                let text = strong.text().collect::<String>();
                if text.contains("演員") {
                    if let Some(value) = panel.select(&value_selector).next() {
                        let actors: Vec<Actor> = value
                            .select(&actor_link_selector)
                            .map(|el| Actor {
                                name: el.text().collect::<String>().trim().to_string(),
                                role: None,
                                thumb: None,
                            })
                            .collect();

                        if !actors.is_empty() {
                            return actors;
                        }
                    }
                }
            }
        }

        // Default to amateur if no actors found for FC2
        vec![]
    }

    fn extract_tags(&self, document: &Html) -> Option<Vec<String>> {
        let panel_selector = Selector::parse(".panel-block").unwrap();
        let strong_selector = Selector::parse("strong").unwrap();
        let value_selector = Selector::parse(".value").unwrap();
        let tag_selector = Selector::parse("a").unwrap();

        for panel in document.select(&panel_selector) {
            if let Some(strong) = panel.select(&strong_selector).next() {
                let text = strong.text().collect::<String>();
                if text.contains("類別") {
                    if let Some(value) = panel.select(&value_selector).next() {
                        let tags: Vec<String> = value
                            .select(&tag_selector)
                            .map(|el| el.text().collect::<String>().trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect();

                        if !tags.is_empty() {
                            return Some(tags);
                        }
                    }
                }
            }
        }

        None
    }

    fn extract_genres(&self, document: &Html) -> Option<Vec<String>> {
        self.extract_tags(document)
    }

    fn extract_number(&self, document: &Html, fallback: &str) -> Result<String> {
        let panel_selector = Selector::parse(".panel-block").unwrap();
        let strong_selector = Selector::parse("strong").unwrap();
        let value_selector = Selector::parse(".value").unwrap();
        let span_selector = Selector::parse("span").unwrap();
        let a_selector = Selector::parse("a").unwrap();

        for panel in document.select(&panel_selector) {
            if let Some(strong) = panel.select(&strong_selector).next() {
                let text = strong.text().collect::<String>();
                if text.contains("番號") {
                    if let Some(value) = panel.select(&value_selector).next() {
                        let mut parts = Vec::new();

                        // Get text from span
                        for span in value.select(&span_selector) {
                            parts.push(span.text().collect::<String>().trim().to_string());
                        }

                        // Get text from a
                        for a in value.select(&a_selector) {
                            parts.insert(0, a.text().collect::<String>().trim().to_string());
                        }

                        let number = parts.join("");
                        if !number.is_empty() {
                            return Ok(number);
                        }
                    }
                }
            }
        }

        Ok(fallback.to_string())
    }

    fn extract_release_date(&self, document: &Html) -> Option<String> {
        let panel_selector = Selector::parse(".panel-block").unwrap();
        let strong_selector = Selector::parse("strong").unwrap();
        let value_selector = Selector::parse(".value").unwrap();
        let span_selector = Selector::parse("span").unwrap();

        for panel in document.select(&panel_selector) {
            if let Some(strong) = panel.select(&strong_selector).next() {
                let text = strong.text().collect::<String>();
                if text.contains("日期") {
                    if let Some(value) = panel.select(&value_selector).next() {
                        if let Some(span) = value.select(&span_selector).next() {
                            let date = span.text().collect::<String>().trim().to_string();
                            if !date.is_empty() {
                                return Some(date);
                            }
                        }
                    }
                }
            }
        }

        None
    }

    fn extract_label(&self, document: &Html) -> Option<String> {
        let panel_selector = Selector::parse(".panel-block").unwrap();
        let strong_selector = Selector::parse("strong").unwrap();
        let value_selector = Selector::parse(".value").unwrap();
        let span_selector = Selector::parse("span").unwrap();
        let a_selector = Selector::parse("a").unwrap();

        for panel in document.select(&panel_selector) {
            if let Some(strong) = panel.select(&strong_selector).next() {
                let text = strong.text().collect::<String>();
                if text.contains("系列") {
                    if let Some(value) = panel.select(&value_selector).next() {
                        // Try to get from <a> first
                        if let Some(a) = value.select(&a_selector).next() {
                            let label = a.text().collect::<String>().trim().to_string();
                            if !label.is_empty() {
                                return Some(label);
                            }
                        }

                        // Try to get from <span>
                        if let Some(span) = value.select(&span_selector).next() {
                            let label = span.text().collect::<String>().trim().to_string();
                            if !label.is_empty() {
                                return Some(label);
                            }
                        }
                    }
                }
            }
        }

        None
    }
}

#[async_trait::async_trait]
impl crate::Scraper for JavdbScraper {
    async fn scrape(&self, number: &str) -> Result<Movie> {
        self.scrape(number).await
    }
}
