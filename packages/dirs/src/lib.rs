use std::{env, path::PathBuf, sync::LazyLock};

use directories::ProjectDirs;

pub static DIR: LazyLock<ProjectDirs> = LazyLock::new(|| ProjectDirs::from("", "", "kk").unwrap());

pub static SEARCH_PATH: LazyLock<PathBuf> = LazyLock::new(|| {
    let search_path = env::var("KK_SEARCH_PATH").expect("KK_SEARCH_PATH env variable is required");
    PathBuf::from(search_path)
});
