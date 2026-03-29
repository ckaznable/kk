use crate::Movie;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf};

/// A single entry in `kk_cache.json`: the scraped metadata keyed by
/// the normalised movie number (upper-case, hyphens removed).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct KkCache {
    /// Maps normalised movie-number → Movie metadata.
    pub movies: HashMap<String, Movie>,
}

impl KkCache {
    pub fn cache_path() -> PathBuf {
        dirs::DIR.config_local_dir().join("kk_cache.json")
    }

    /// Load the cache from disk (returns an empty cache on any error).
    pub fn load() -> Self {
        let path = Self::cache_path();
        if !path.exists() {
            return Self::default();
        }
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Look up a movie by its number (comparison is normalised so both
    /// "SSIS-123" and "SSIS123" will match the same entry).
    pub fn find(&self, number: &str) -> Option<&Movie> {
        let needle = normalise_key(number);
        self.movies
            .iter()
            .find(|(k, _)| normalise_key(k) == needle)
            .map(|(_, v)| v)
    }

    /// Insert / overwrite an entry and immediately persist to disk.
    /// The key is stored as-is (with dashes preserved) so that the JSON
    /// file remains human-readable (e.g. "SSIS-123" instead of "SSIS123").
    pub fn insert_and_flush(&mut self, number: &str, movie: Movie) {
        let needle = normalise_key(number);
        self.movies.retain(|k, _| normalise_key(k) != needle);
        self.movies.insert(number.to_string(), movie);
        self.flush();
    }

    pub fn flush(&self) {
        let path = Self::cache_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        if let Ok(content) = serde_json::to_string(self) {
            std::fs::write(path, content).ok();
        }
    }
}

/// Normalise a movie-number key: upper-case, strip hyphens and underscores.
fn normalise_key(number: &str) -> String {
    number.to_uppercase().replace('-', "").replace('_', "")
}

#[derive(Deserialize, Debug)]
pub struct CacheRequest {
    /// Scraper type: "javdb" or "fc2".
    #[serde(rename = "type")]
    pub scraper_type: String,
    /// The movie number / 番號 (e.g. "SSIS-123" or "FC2-12345").
    pub num: String,
    /// Raw HTML from the detail page.
    pub html: String,
}
