use ahash::AHashSet;
use anyhow::Result;
use log::warn;
use rand::rng;
use rand::seq::SliceRandom;
use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};

use serde::{Deserialize, Serialize};

use crate::{Movie, util::find_new_movie_nfo};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    pub movies: Vec<MovieData>,
    pub last_scan_time: SystemTime,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            movies: Default::default(),
            last_scan_time: SystemTime::UNIX_EPOCH,
        }
    }
}

#[derive(Debug)]
pub struct IndexCacheTable {
    pub idx: Option<Vec<u32>>,
    pub dirty: bool,
}

impl Default for IndexCacheTable {
    fn default() -> Self {
        Self {
            idx: None,
            dirty: true,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MovieData {
    pub path: PathBuf,
    pub movie: Movie,
    pub added_time: SystemTime,
    pub fav: bool,
    #[serde(default)]
    pub markers: Vec<f64>,
}

impl MovieData {
    pub fn abs_path(&self) -> PathBuf {
        if self.path.is_absolute() {
            self.path.clone()
        } else {
            dirs::SEARCH_PATH.join(&self.path)
        }
    }

    pub fn abs_thumb_path(&self) -> Option<PathBuf> {
        let thumb = self.movie.thumb.as_ref()?;
        let p = PathBuf::from(thumb);
        if p.is_absolute() {
            return Some(p);
        }

        // Try cache first
        let cache_path = dirs::THUMB_CACHE_DIR.join(&p);
        if cache_path.exists() {
            return Some(cache_path);
        }

        // Fallback to NFO relative path
        let nfo_abs = self.abs_path();
        let nfo_relative = nfo_abs.parent()?.join(&p);
        if nfo_relative.exists() {
            return Some(nfo_relative);
        }

        Some(cache_path) // return cache path even if not exists as a default
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct WebDavMovieData {
    pub url_path: String, // Path relative to WebDAV base
    pub movie: Movie,
    pub added_time: SystemTime,
    pub fav: bool,
    #[serde(default)]
    pub markers: Vec<f64>,
}

impl WebDavMovieData {
    pub fn abs_thumb_path(&self) -> Option<PathBuf> {
        let thumb = self.movie.thumb.as_ref()?;
        let p = PathBuf::from(thumb);
        if p.is_absolute() {
            return Some(p);
        }
        Some(dirs::THUMB_CACHE_DIR.join(&p))
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct WebDavConfig {
    pub base_url: String,
    #[serde(skip)]
    pub user: Option<String>,
    #[serde(skip)]
    pub pass: Option<String>,
    pub movies: Vec<WebDavMovieData>,
}

#[derive(Debug, Default)]
pub struct WebDavDatabase {
    pub config: WebDavConfig,
}

impl WebDavDatabase {
    pub fn new() -> Result<Self> {
        let config_path = Self::config_path();
        let mut config: WebDavConfig = if !config_path.exists() {
            WebDavConfig::default()
        } else {
            let content = std::fs::read_to_string(config_path)?;
            serde_json::from_str(&content)?
        };

        if config.base_url.is_empty() {
            if let Some(url) = dirs::WEBDAV_URL.clone() {
                config.base_url = url;
            }
        }
        if config.user.is_none() {
            config.user = dirs::WEBDAV_USER.clone();
        }
        if config.pass.is_none() {
            config.pass = dirs::WEBDAV_PASS.clone();
        }

        Ok(Self { config })
    }

    #[inline]
    fn config_path() -> PathBuf {
        dirs::DIR.config_local_dir().join("kwa_db.json")
    }

    pub fn flush(&self) {
        let config_path = Self::config_path();
        if let Ok(content) = serde_json::to_string(&self.config) {
            std::fs::write(config_path, content).ok();
        }
    }

    pub fn get_movie(&self, i: usize) -> Option<&WebDavMovieData> {
        self.config.movies.get(i)
    }

    pub fn toggle_fav(&mut self, i: usize) -> bool {
        if let Some(movie) = self.config.movies.get_mut(i) {
            movie.fav = !movie.fav;
            return movie.fav;
        }
        false
    }

    pub fn contains_id(&self, id: &str) -> bool {
        self.find_movie_by_id(id).is_some()
    }

    pub fn find_movie_by_id(&self, id: &str) -> Option<&Movie> {
        let clean_id = id.to_uppercase().replace("-", "").replace("_", "");
        self.config.movies.iter().find_map(|m| {
            if let Some(ref num) = m.movie.num {
                let clean_num = num.to_uppercase().replace("-", "").replace("_", "");
                if clean_num == clean_id {
                    return Some(&m.movie);
                }
            }
            None
        })
    }
}

#[derive(Debug)]
pub struct SimpleJsonDatabase {
    pub config: Config,
    index_ref: Vec<u32>,
    order_by_fav_index: IndexCacheTable,
    order_by_added_time_index: IndexCacheTable,
    order_by_random_index: IndexCacheTable,
    order_by_marked_index: IndexCacheTable,
}

impl Default for SimpleJsonDatabase {
    fn default() -> Self {
        let config = Config::default();
        let index_ref = (0..config.movies.len() as u32).collect();

        Self {
            config,
            index_ref,
            order_by_fav_index: IndexCacheTable::default(),
            order_by_added_time_index: IndexCacheTable::default(),
            order_by_random_index: IndexCacheTable::default(),
            order_by_marked_index: IndexCacheTable::default(),
        }
    }
}

impl SimpleJsonDatabase {
    pub fn new() -> Result<Self> {
        let mut def = Self::default();
        def.config = Self::init_config()?;
        Ok(def)
    }

    pub fn load_config(&mut self, p: &Path) -> Result<()> {
        self.config = Self::load(p)?;
        self.index_ref = (0..self.config.movies.len() as u32).collect();
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Config> {
        let mut config = Self::init_config()?;
        let known_files: AHashSet<PathBuf> =
            config.movies.iter().map(|item| item.abs_path()).collect();

        let now = SystemTime::now();
        let new_nfos = find_new_movie_nfo(path, config.last_scan_time, &known_files)?;
        let new_list_iter = new_nfos
            .into_iter()
            .flat_map(|p| Self::load_movie_from_nfo(&p, now));

        config.movies.extend(new_list_iter);
        config.last_scan_time = SystemTime::now();

        // update cache
        if let Ok(content) = serde_json::to_string(&config) {
            std::fs::write(Self::config_path(), content).ok();
        }

        Ok(config)
    }

    #[inline]
    fn config_path() -> PathBuf {
        dirs::DIR.config_local_dir().join("kr.json")
    }

    pub fn load_movie_from_nfo(path: &Path, added_time: SystemTime) -> Option<MovieData> {
        if !path.exists() {
            return None;
        }

        let nfo = std::fs::read_to_string(path).ok()?;
        let Ok(movie) = quick_xml::de::from_str(&nfo) else {
            warn!("{path:?} nfo parse failed");
            return None;
        };

        let path = path
            .strip_prefix(&*dirs::SEARCH_PATH)
            .unwrap_or(path)
            .to_owned();

        Some(MovieData {
            path,
            movie,
            added_time,
            fav: false,
            markers: Vec::new(),
        })
    }

    pub fn init_config() -> Result<Config> {
        let config_path = Self::config_path();
        if !config_path.exists() {
            std::fs::create_dir_all(config_path.parent().unwrap())?;
            Ok(Config::default())
        } else {
            let content = std::fs::read_to_string(config_path)?;
            Ok(serde_json::from_str(&content)?)
        }
    }

    pub fn reload(&mut self) {
        if let Ok(config) = Self::init_config() {
            self.config = config;
            self.order_by_fav_index.dirty = true;
            self.order_by_added_time_index.dirty = true;
            self.order_by_random_index.dirty = true;
            self.order_by_marked_index.dirty = true;
            self.index_ref = (0..self.config.movies.len() as u32).collect();
        }
    }

    pub fn flush(&self) {
        let config_path = Self::config_path();
        if let Ok(content) = serde_json::to_string(&self.config) {
            std::fs::write(config_path, content).ok();
        }
    }

    pub fn filter_by_fav<'a>(&'a mut self) -> DatabaseSlice<'a> {
        if !self.order_by_fav_index.dirty
            && let Some(ref idx) = self.order_by_fav_index.idx
        {
            return DatabaseSlice::new(&self.config.movies, idx);
        }

        self.order_by_fav_index.dirty = false;
        let data: Vec<u32> = self
            .index_ref
            .iter()
            .copied()
            .filter(|i| {
                self.config
                    .movies
                    .get(*i as usize)
                    .map(|d| d.fav)
                    .unwrap_or(false)
            })
            .collect();

        let index = self.order_by_fav_index.idx.insert(data);
        DatabaseSlice::new(&self.config.movies, index)
    }

    pub fn filter_by_marked<'a>(&'a mut self) -> DatabaseSlice<'a> {
        if !self.order_by_marked_index.dirty
            && let Some(ref idx) = self.order_by_marked_index.idx
        {
            return DatabaseSlice::new(&self.config.movies, idx);
        }

        self.order_by_marked_index.dirty = false;
        let data: Vec<u32> = self
            .index_ref
            .iter()
            .copied()
            .filter(|i| {
                self.config
                    .movies
                    .get(*i as usize)
                    .map(|d| !d.markers.is_empty())
                    .unwrap_or(false)
            })
            .collect();

        let index = self.order_by_marked_index.idx.insert(data);
        DatabaseSlice::new(&self.config.movies, index)
    }

    pub fn order_by_random<'a>(&'a mut self) -> DatabaseSlice<'a> {
        if !self.order_by_random_index.dirty
            && let Some(ref idx) = self.order_by_random_index.idx
        {
            return DatabaseSlice::new(&self.config.movies, idx);
        }

        self.order_by_random_index.dirty = false;
        let mut data = self.index_ref.clone();

        let mut rng = rng();
        data.shuffle(&mut rng);

        let index = self.order_by_random_index.idx.insert(data);
        DatabaseSlice::new(&self.config.movies, index)
    }

    pub fn order_by_added_time<'a>(&'a mut self) -> DatabaseSlice<'a> {
        if !self.order_by_added_time_index.dirty
            && let Some(ref idx) = self.order_by_added_time_index.idx
        {
            return DatabaseSlice::new(&self.config.movies, idx);
        }

        self.order_by_added_time_index.dirty = false;
        let mut data: Vec<u32> = self.index_ref.to_vec();
        data.sort_unstable_by(|a, b| b.cmp(a));

        let index = self.order_by_added_time_index.idx.insert(data);
        DatabaseSlice::new(&self.config.movies, index)
    }

    pub fn get_movie(&self, i: usize) -> Option<&MovieData> {
        self.config.movies.get(i)
    }

    pub fn find_movie_by_id(&self, id: &str) -> Option<&Movie> {
        let clean_id = id.to_uppercase().replace("-", "").replace("_", "");
        self.config.movies.iter().find_map(|m| {
            if let Some(ref num) = m.movie.num {
                let clean_num = num.to_uppercase().replace("-", "").replace("_", "");
                if clean_num == clean_id {
                    return Some(&m.movie);
                }
            }
            None
        })
    }

    /// Toggle the favorite status of a movie at the given index
    pub fn toggle_fav(&mut self, i: usize) -> bool {
        if let Some(movie) = self.config.movies.get_mut(i) {
            movie.fav = !movie.fav;
            // Mark the fav index as dirty so it will be recalculated
            self.order_by_fav_index.dirty = true;
            return movie.fav;
        }
        false
    }

    /// Get the list of actor names for a movie at the given index
    pub fn get_actors(&self, i: usize) -> Vec<String> {
        self.config
            .movies
            .get(i)
            .map(|m| m.movie.actor.iter().map(|a| a.name.clone()).collect())
            .unwrap_or_default()
    }

    /// Filter movies by actor name
    pub fn filter_by_actor<'a>(&'a self, actor_name: &str) -> Vec<u32> {
        self.index_ref
            .iter()
            .copied()
            .filter(|i| {
                self.config
                    .movies
                    .get(*i as usize)
                    .map(|d| d.movie.actor.iter().any(|a| a.name == actor_name))
                    .unwrap_or(false)
            })
            .collect()
    }

    /// Get the markers for a movie at the given index
    pub fn get_markers(&self, i: usize) -> Vec<f64> {
        self.config
            .movies
            .get(i)
            .map(|m| m.markers.clone())
            .unwrap_or_default()
    }

    /// Add or remove a marker (timestamp) for a movie at the given index.
    /// If the new marker is within 1 second of an existing marker, the existing
    /// marker is removed (toggle behavior). Otherwise the new marker is added.
    pub fn add_marker(&mut self, i: usize, time: f64) -> bool {
        if let Some(movie) = self.config.movies.get_mut(i) {
            // Check if there's an existing marker within 1 second
            if let Some(pos) = movie.markers.iter().position(|&m| (m - time).abs() < 1.0) {
                // Toggle: remove the existing marker
                movie.markers.remove(pos);
                self.order_by_marked_index.dirty = true;
                return false; // marker removed
            }

            movie.markers.push(time);
            movie
                .markers
                .sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            self.order_by_marked_index.dirty = true;
            return true; // marker added
        }
        false
    }
}

#[derive(Clone)]
pub struct IndexedMovieData<'a> {
    pub movie: &'a MovieData,
    pub index: u32,
}

pub struct DatabaseSlice<'a> {
    items: &'a [MovieData],
    index: &'a [u32],
    i: usize,
}

impl<'a> DatabaseSlice<'a> {
    pub fn new(items: &'a [MovieData], index: &'a [u32]) -> Self {
        Self { items, index, i: 0 }
    }
}

impl<'a> Iterator for DatabaseSlice<'a> {
    type Item = IndexedMovieData<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        self.i += 1;
        self.index.get(self.i - 1).and_then(|i| {
            let movie = self.items.get(*i as usize)?;
            Some(IndexedMovieData { movie, index: *i })
        })
    }
}
