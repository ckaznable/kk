use std::sync::LazyLock;

use directories::ProjectDirs;

pub static DIR: LazyLock<ProjectDirs> = LazyLock::new(|| {
    ProjectDirs::from("", "", "kk").unwrap()
});

