use ahash::AHashSet;
use anyhow::Result;
use std::{
    path::{Path, PathBuf}, time::SystemTime
};

use serde::{Deserialize, Serialize};

use crate::{Movie, util::find_new_movie_dirs};

#[derive(Serialize, Deserialize, Clone)]
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

pub struct IndexCacheTable {
    pub idx: Option<Vec<u32>>,
    pub dirty: bool,
}

impl Default for IndexCacheTable {
    fn default() -> Self {
        Self {
            idx: Default::default(),
            dirty: true
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct MovieData {
    pub path: PathBuf,
    pub movie: Movie,
    pub added_time: u64,
    pub fav: bool,
}

pub struct SimpleJsonDatabase {
    config: Config,
    index_ref: Vec<u32>,
    order_by_fav_index: IndexCacheTable,
    order_by_added_time_index: IndexCacheTable,
}

impl SimpleJsonDatabase {
    pub fn new(path: PathBuf) -> Result<Self> {
        let config = Self::load(&path)?;
        let index_ref = (0..config.movies.len() as u32).collect();

        Ok(Self {
            config,
            index_ref,
            order_by_fav_index: IndexCacheTable::default(),
            order_by_added_time_index: IndexCacheTable::default(),
        })
    }

    pub fn load(path: &Path) -> Result<Config> {
        let mut config = Self::init_config()?;
        let known_files: AHashSet<PathBuf> =
            config.movies.iter().map(|item| item.path.clone()).collect();

        let new_dirs = find_new_movie_dirs(path, config.last_scan_time, &known_files)?;
        let new_list_iter = new_dirs
            .into_iter()
            .flat_map(|p| Self::load_movie_from_nfo(&p));

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

    pub fn load_movie_from_nfo(path: &Path) -> Option<MovieData> {
        let movie_name = path.file_name()?.to_string_lossy();
        let nfo = path.join(format!("{movie_name}.nfo"));
        if !nfo.exists() {
            return None;
        }

        let nfo = std::fs::read_to_string(nfo).ok()?;
        let nfo: MovieData = quick_xml::de::from_str(&nfo).ok()?;
        Some(nfo)
    }

    pub fn init_config() -> Result<Config> {
        let config_path = Self::config_path();
        if !config_path.exists() {
            std::fs::create_dir_all(config_path)?;
            Ok(Config::default())
        } else {
            let content = std::fs::read_to_string(config_path)?;
            Ok(serde_json::from_str(&content)?)
        }
    }

    pub fn reload(&mut self) {
        if let Ok(config) = Self::load(&Self::config_path()) {
            self.config = config;
            self.order_by_fav_index.dirty = true;
            self.order_by_added_time_index.dirty = true;
            self.index_ref = (0..self.config.movies.len() as u32).collect();
        }
    }

    pub fn flush(&self) {
        let config_path = Self::config_path();
        if let Ok(content) = serde_json::to_string(&self.config) {
            std::fs::write(config_path, content).ok();
        }
    }

    pub fn order_by_fav<'a>(&'a mut self) -> DatabaseSlice<'a> {
        if !self.order_by_fav_index.dirty && let Some(ref idx) = self.order_by_fav_index.idx {
            return DatabaseSlice::new(&self.config.movies, idx);
        }

        self.order_by_fav_index.dirty = false;
        let data: Vec<u32> = self
            .index_ref
            .iter()
            .copied()
            .filter(|i| {
                if let Some(d) = self.config.movies.get(*i as usize) {
                    d.fav
                } else {
                    false
                }
            })
            .collect();

        let index = self.order_by_fav_index.idx.insert(data);
        DatabaseSlice::new(&self.config.movies, index)
    }

    pub fn order_by_added_time<'a>(&'a mut self) -> DatabaseSlice<'a> {
        if !self.order_by_added_time_index.dirty && let Some(ref idx) = self.order_by_added_time_index.idx {
            return DatabaseSlice::new(&self.config.movies, idx);
        }

        self.order_by_added_time_index.dirty = false;
        let mut data: Vec<u32> = self.index_ref.to_vec();
        data.sort_unstable_by(|a, b| b.cmp(a));

        let index = self.order_by_added_time_index.idx.insert(data);
        DatabaseSlice::new(&self.config.movies, index)
    }
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
    type Item = &'a MovieData;

    fn next(&mut self) -> Option<Self::Item> {
        self.i += 1;
        self.index
            .get(self.i - 1)
            .and_then(|i| self.items.get(*i as usize))
    }
}
