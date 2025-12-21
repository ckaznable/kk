use serde::{Deserialize, Serialize};

pub mod db;
pub mod util;

#[derive(Serialize, Deserialize, Clone)]
pub struct Movie {
    pub title: String,
    pub outline: Option<String>,
    pub poster: Option<String>,
    pub thumb: Option<String>,
    pub fanart: Option<String>,
    pub label: Option<String>,
    pub actor: Vec<Actor>,
    pub tag: Option<Vec<String>>,
    pub genre: Option<Vec<String>>,
    pub num: Option<String>,
    pub releasedate: Option<String>,
    pub cover: Option<String>,
    pub website: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Actor {
    pub name: String,
    pub role: Option<String>,
    pub thumb: Option<String>,
}

