use anyhow::{anyhow, Result};
use kr::{Actor, Movie};
use reqwest::blocking::Client;
use scraper::{Html, Selector};

pub struct Fc2Scraper {
    client: Client,
}

impl Fc2Scraper {
    pub fn new() -> Result<Self> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36".parse()?,
        );

        let client = Client::builder()
            .cookie_store(true)
            .default_headers(headers)
            .build()?;

        Ok(Self { client })
    }

    fn normalize_number(&self, number: &str) -> String {
        number
            .to_lowercase()
            .replace("fc2-ppv-", "")
            .replace("fc2-", "")
    }

    pub fn scrape(&self, number: &str) -> Result<Movie> {
        let normalized_number = self.normalize_number(number);
        let detail_url = format!(
            "https://adult.contents.fc2.com/article/{}/",
            normalized_number
        );

        let response = self.client.get(&detail_url).send()?;

        if response.status().as_u16() == 404 {
            return Err(anyhow!("Movie not found: FC2-{}", normalized_number));
        }

        let html = response.text()?;
        let document = Html::parse_document(&html);

        // Extract data
        let title = self.extract_title(&document)?;
        let outline = self.extract_outline(&document);
        let cover = self.extract_cover(&document);
        let actors = self.extract_actors(&document);
        let tags = self.extract_tags(&document);
        let releasedate = self.extract_release_date(&document);
        let num = format!("FC2-{}", normalized_number);

        Ok(Movie {
            title,
            outline,
            poster: cover.clone(),
            thumb: cover.clone(),
            fanart: cover.clone(),
            label: None,
            actor: actors,
            tag: tags.clone(),
            genre: tags,
            num: Some(num),
            releasedate,
            cover,
            website: Some(detail_url),
        })
    }

    fn extract_title(&self, document: &Html) -> Result<String> {
        let title_selector = Selector::parse("title").unwrap();

        if let Some(title_el) = document.select(&title_selector).next() {
            let title = title_el.text().collect::<String>().trim().to_string();
            Ok(title)
        } else {
            Err(anyhow!("Title not found"))
        }
    }

    fn extract_outline(&self, document: &Html) -> Option<String> {
        // FC2 might have description, but it's not always available
        let desc_selector = Selector::parse(".items_article_Desc").unwrap();

        document
            .select(&desc_selector)
            .next()
            .map(|el| el.text().collect::<String>().trim().to_string())
            .filter(|s| !s.is_empty())
    }

    fn extract_cover(&self, document: &Html) -> Option<String> {
        let cover_selector = Selector::parse(".items_article_MainitemThumb span img").unwrap();

        if let Some(img) = document.select(&cover_selector).next() {
            if let Some(src) = img.value().attr("src") {
                // Make absolute URL
                if src.starts_with("//") {
                    return Some(format!("https:{}", src));
                } else if src.starts_with("/") {
                    return Some(format!("https://adult.contents.fc2.com{}", src));
                } else if !src.starts_with("http") {
                    return Some(format!("https://adult.contents.fc2.com/{}", src));
                }
                return Some(src.to_string());
            }
        }

        None
    }

    fn extract_actors(&self, document: &Html) -> Vec<Actor> {
        // FC2 videos typically feature "素人" (amateur) performers
        // Try to extract from the page, but default to amateur if not found

        let actor_selector = Selector::parse("#top > div.container > section > div > section > div.items_article_headerInfo > ul > li:nth-child(3) > a").unwrap();

        let actors: Vec<Actor> = document
            .select(&actor_selector)
            .map(|el| Actor {
                name: el.text().collect::<String>().trim().to_string(),
                role: None,
                thumb: None,
            })
            .filter(|a| !a.name.is_empty())
            .collect();

        if actors.is_empty() {
            vec![Actor {
                name: "素人".to_string(),
                role: None,
                thumb: None,
            }]
        } else {
            actors
        }
    }

    fn extract_tags(&self, document: &Html) -> Option<Vec<String>> {
        let tag_selector = Selector::parse("a.tag.tagTag").unwrap();

        let tags: Vec<String> = document
            .select(&tag_selector)
            .map(|el| el.text().collect::<String>().trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        if tags.is_empty() {
            None
        } else {
            Some(tags)
        }
    }

    fn extract_release_date(&self, document: &Html) -> Option<String> {
        let date_selector = Selector::parse("#top > div.container > section > div > section > div.items_article_headerInfo > div.items_article_Releasedate > p").unwrap();

        if let Some(date_el) = document.select(&date_selector).next() {
            let date_text = date_el.text().collect::<String>();
            // The text might be like "販売日 : 2023/01/15"
            let date = date_text
                .trim()
                .trim_start_matches("販売日")
                .trim_start_matches(":")
                .trim()
                .replace("/", "-");

            if !date.is_empty() {
                return Some(date);
            }
        }

        None
    }
}

impl crate::Scraper for Fc2Scraper {
    fn scrape(&self, number: &str) -> Result<Movie> {
        self.scrape(number)
    }
}
