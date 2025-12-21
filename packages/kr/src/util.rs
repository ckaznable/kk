use std::{
    fs::{self, DirEntry},
    path::{Path, PathBuf},
    time::SystemTime,
};

use ahash::AHashSet;

pub fn find_new_movie_dirs(
    root: &Path,
    last_scan_time: SystemTime,
    known_files: &AHashSet<PathBuf>,
) -> std::io::Result<Vec<PathBuf>> {
    let mut new_files = Vec::new();

    let entries = fs::read_dir(root)?;
    let mut entries: Vec<DirEntry> = entries.into_iter().flatten().collect();

    // hdd friendly
    entries.sort_by_key(|e| {
        e.metadata()
            .map(|m| {
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    m.file_index().unwrap_or(0)
                }

                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    m.ino()
                }

                #[cfg(not(any(unix, windows)))]
                {
                    0
                }
            })
            .unwrap_or(0)
    });

    for entry in entries {
        let path = entry.path();

        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let file_type = metadata.file_type();

        if file_type.is_dir() {
            let Ok(dir_mtime) = metadata.modified() else {
                continue;
            };

            if dir_mtime <= last_scan_time {
                continue;
            }

            if let Ok(sub_entries) = fs::read_dir(&path) {
                let sub_entries = sub_entries.into_iter();
                for sub_entry in sub_entries {
                    let Ok(sub_entry) = sub_entry else {
                        continue;
                    };
                    let sub_path = sub_entry.path();
                    if !sub_path.is_file() && !known_files.contains(&sub_path) {
                        new_files.push(sub_path);
                    }
                }
            }
        }
    }

    Ok(new_files)
}
