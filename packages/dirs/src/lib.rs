use std::{env, path::PathBuf, sync::LazyLock};

use directories::ProjectDirs;

pub static DIR: LazyLock<ProjectDirs> = LazyLock::new(|| ProjectDirs::from("", "", "kk").unwrap());

pub static SEARCH_PATH: LazyLock<PathBuf> = LazyLock::new(|| {
    let search_path = env::var("KK_SEARCH_PATH").expect("KK_SEARCH_PATH env variable is required");
    PathBuf::from(search_path)
});

pub static WEBDAV_URL: LazyLock<Option<String>> = LazyLock::new(|| env::var("KK_WEBDAV_URL").ok());
pub static WEBDAV_USER: LazyLock<Option<String>> = LazyLock::new(|| env::var("KK_WEBDAV_USER").ok());
pub static WEBDAV_PASS: LazyLock<Option<String>> = LazyLock::new(|| env::var("KK_WEBDAV_PASS").ok());

pub static JAVDB_COOKIE: LazyLock<Option<String>> = LazyLock::new(|| env::var("KK_JAVDB_COOKIE").ok());
