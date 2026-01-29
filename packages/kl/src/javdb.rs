use anyhow::{anyhow, Result};
use kr::{Actor, Movie};
use reqwest::blocking::Client;
use scraper::{Html, Selector};

pub struct JavdbScraper {
    client: Client,
    site: String,
}

impl JavdbScraper {
    pub fn new() -> Result<Self> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/123.0.0.0 Safari/537.36".parse()?,
        );

        let client = Client::builder()
            .cookie_store(true)
            .default_headers(headers)
            .build()?;

        Ok(Self {
            client,
            site: "javdb".to_string(),
        })
    }

    pub fn with_site(site: String) -> Result<Self> {
        let mut scraper = Self::new()?;
        scraper.site = site;
        Ok(scraper)
    }

    fn search_movie(&self, number: &str) -> Result<String> {
        let search_url = format!("https://{}.com/search?q={}&f=all", self.site, number);

        // Set cookies
        let cookie_url = format!("https://{}.com", self.site);
        self.client
            .get(&cookie_url)
            .header("Cookie", "over18=1; theme=auto; locale=zh")
            .send()?;

        let response = self
            .client
            .get(&search_url)
            .header("Cookie", "over18=1; theme=auto; locale=zh")
            .send()?;

        let html = response.text()?;
        let document = Html::parse_document(&html);

        // Find the correct movie URL from search results
        let url_selector = Selector::parse(".movie-list > div > a").unwrap();
        let title_selector = Selector::parse(".video-title strong").unwrap();

        let urls: Vec<String> = document
            .select(&url_selector)
            .filter_map(|el| el.value().attr("href"))
            .map(|s| s.to_string())
            .collect();

        let titles: Vec<String> = document
            .select(&title_selector)
            .map(|el| el.text().collect::<String>())
            .collect();

        // Find matching URL
        let correct_url = if let Some(pos) = titles
            .iter()
            .position(|t| t.to_uppercase() == number.to_uppercase())
        {
            urls.get(pos)
                .ok_or_else(|| anyhow!("URL not found for number: {}", number))?
        } else if !urls.is_empty()
            && !titles.is_empty()
            && titles[0].to_uppercase() == number.to_uppercase()
        {
            &urls[0]
        } else {
            return Err(anyhow!("Movie not found in javdb: {}", number));
        };

        Ok(format!("https://{}.com{}", self.site, correct_url))
    }

    pub fn scrape(&self, number: &str) -> Result<Movie> {
        let detail_url = self.search_movie(number)?;

        let response = self
            .client
            .get(&detail_url)
            .header("Cookie", "over18=1; theme=auto; locale=zh")
            .send()?;

        let html = response.text()?;

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

impl crate::Scraper for JavdbScraper {
    fn scrape(&self, number: &str) -> Result<Movie> {
        self.scrape(number)
    }
}
