use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::Movie;

const DIRTY_FAV: u8 = 1;
const DIRTY_ADDED_TIME: u8 = 1 << 1;

#[derive(Clone, Copy)]
struct DirtyMark(u8);

impl Default for DirtyMark {
    fn default() -> Self {
        Self(255)
    }
}

impl DirtyMark {
    pub fn dirty(&mut self) {
        self.0 = 255;
    }

    pub fn clear(&mut self, mask: u8) {
        self.0 &= !mask;
    }

    pub fn is_dirty(&self, mask: u8) -> bool {
        self.0 & mask != 0
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
    path: PathBuf,
    items: Vec<MovieData>,
    dirty: DirtyMark,
    page_size: usize,
    index_ref: Vec<u32>,
    order_by_fav_index: Option<Vec<u32>>,
    order_by_added_time_index: Option<Vec<u32>>,
}

impl SimpleJsonDatabase {
    pub fn new() -> Self {
        todo!()
    }

    pub fn rebuild_index_ref(&mut self) {
        self.index_ref = (0..self.items.len() as u32).collect();
    }

    pub fn reload(&mut self) {
        self.dirty.dirty();
        self.rebuild_index_ref();
        todo!()
    }

    pub fn flush(&self) {
        todo!()
    }

    pub fn order_by_fav<'a>(&'a mut self) -> SimpleJsonDatabaseSlice<'a> {
        if !self.dirty.is_dirty(DIRTY_FAV) && let Some(ref idx) = self.order_by_fav_index {
            return SimpleJsonDatabaseSlice::new(&self.items, idx);
        }

        self.dirty.clear(DIRTY_FAV);
        let data: Vec<u32> = self
            .index_ref
            .iter()
            .copied()
            .filter(|i| {
                if let Some(d) = self.items.get(*i as usize) {
                    d.fav
                } else {
                    false
                }
            })
            .collect();

        let index = self.order_by_fav_index.insert(data);
        SimpleJsonDatabaseSlice::new(&self.items, index)
    }

    pub fn order_by_added_time<'a>(&'a mut self) -> SimpleJsonDatabaseSlice<'a> {
        if !self.dirty.is_dirty(DIRTY_ADDED_TIME) && let Some(ref idx) = self.order_by_added_time_index {
            return SimpleJsonDatabaseSlice::new(&self.items, idx);
        }

        self.dirty.clear(DIRTY_ADDED_TIME);
        let mut data: Vec<u32> = self.index_ref.to_vec();
        data.sort_unstable_by(|a, b| b.cmp(a));

        let index = self.order_by_added_time_index.insert(data);
        SimpleJsonDatabaseSlice::new(&self.items, index)
    }
}

pub struct SimpleJsonDatabaseSlice<'a> {
    items: &'a [MovieData],
    index: &'a [u32],
    i: usize,
}

impl<'a> SimpleJsonDatabaseSlice<'a> {
    pub fn new(items: &'a [MovieData], index: &'a [u32]) -> Self {
        Self { items, index, i: 0 }
    }
}

impl<'a> Iterator for SimpleJsonDatabaseSlice<'a> {
    type Item = &'a MovieData;

    fn next(&mut self) -> Option<Self::Item> {
        self.i += 1;
        self.index.get(self.i - 1)
            .and_then(|i| self.items.get(*i as usize))
    }
}
