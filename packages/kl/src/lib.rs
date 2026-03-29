#[cfg(feature = "browser")]
pub mod browser;
pub mod fc2;
pub mod javdb;
pub mod number_parser;

use anyhow::Result;
use kr::Movie;

#[async_trait::async_trait]
pub trait Scraper: Send + Sync {
    async fn scrape(&self, number: &str) -> Result<Movie>;
}

pub fn generate_nfo_xml(movie: &Movie) -> Result<String> {
    use quick_xml::se::to_string;

    #[derive(serde::Serialize)]
    #[serde(rename = "movie")]
    struct Nfo {
        title: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        outline: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        poster: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        thumb: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        fanart: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        actor: Vec<NfoActor>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tag: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        genre: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        num: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        releasedate: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cover: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        website: Option<String>,
    }

    #[derive(serde::Serialize)]
    struct NfoActor {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        role: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        thumb: Option<String>,
    }

    let nfo_actors: Vec<NfoActor> = movie
        .actor
        .iter()
        .map(|a| NfoActor {
            name: a.name.clone(),
            role: a.role.clone(),
            thumb: a.thumb.clone(),
        })
        .collect();

    let nfo = Nfo {
        title: movie.title.clone(),
        outline: movie.outline.clone(),
        poster: movie.poster.clone(),
        thumb: movie.thumb.clone(),
        fanart: movie.fanart.clone(),
        label: movie.label.clone(),
        actor: nfo_actors,
        tag: movie.tag.clone(),
        genre: movie.genre.clone(),
        num: movie.num.clone(),
        releasedate: movie.releasedate.clone(),
        cover: movie.cover.clone(),
        website: movie.website.clone(),
    };

    let xml = to_string(&nfo)?;
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n{}",
        xml
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kr::Actor;

    #[test]
    fn test_generate_nfo_xml() {
        let movie = Movie {
            title: "Test Movie".to_string(),
            outline: Some("Test Outline".to_string()),
            poster: Some("http://example.com/poster.jpg".to_string()),
            thumb: Some("http://example.com/thumb.jpg".to_string()),
            fanart: Some("http://example.com/fanart.jpg".to_string()),
            label: Some("Test Label".to_string()),
            actor: vec![
                Actor {
                    name: "Actor 1".to_string(),
                    role: Some("Role 1".to_string()),
                    thumb: Some("http://example.com/actor1.jpg".to_string()),
                },
                Actor {
                    name: "Actor 2".to_string(),
                    role: None,
                    thumb: None,
                },
            ],
            tag: Some(vec!["Tag1".to_string(), "Tag2".to_string()]),
            genre: Some(vec!["Genre1".to_string()]),
            num: Some("TEST-001".to_string()),
            releasedate: Some("2023-01-01".to_string()),
            cover: Some("http://example.com/cover.jpg".to_string()),
            website: Some("http://example.com/movie".to_string()),
        };

        let xml = generate_nfo_xml(&movie).unwrap();
        println!("{}", xml);

        assert!(xml.contains("<movie>"));
        assert!(xml.contains("<title>Test Movie</title>"));
        assert!(xml.contains("<num>TEST-001</num>"));
        assert!(xml.contains("<actor>"));
        assert!(xml.contains("<name>Actor 1</name>"));
        assert!(xml.contains("</actor>"));
    }
}
