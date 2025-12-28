use std::{
    fs::{self, DirEntry},
    path::{Path, PathBuf},
    time::SystemTime,
};

use ahash::AHashSet;

pub fn find_new_movie_nfo(
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
        get_nfo_files(&mut new_files, entry, known_files, last_scan_time);
    }

    Ok(new_files)
}

fn get_nfo_files(
    buf: &mut Vec<PathBuf>,
    entry: DirEntry,
    known_files: &AHashSet<PathBuf>,
    last_scan_time: SystemTime,
) {
    let path = entry.path();

    let Ok(metadata) = entry.metadata() else {
        return;
    };

    let Ok(dir_mtime) = metadata.modified() else {
        return;
    };

    if dir_mtime <= last_scan_time {
        return;
    }

    let Some(p) = path.parent() else {
        return;
    };

    let Some(dir_name) = p.file_name() else {
        return;
    };

    let Some(file_name) = path.file_name() else {
        return;
    };

    let dir_name = dir_name.to_string_lossy();
    let file_name = file_name.to_string_lossy();

    let file_type = metadata.file_type();
    if file_type.is_file()
        && file_name.starts_with(dir_name.as_ref())
        && is_nfo_file(known_files, &path)
    {
        buf.push(path);
    } else if file_type.is_dir()
        && let Ok(sub_entries) = fs::read_dir(&path)
    {
        for sub_entry in sub_entries.into_iter().flatten() {
            get_nfo_files(buf, sub_entry, known_files, last_scan_time);
        }
    }
}

#[inline]
fn is_nfo_file(known_files: &AHashSet<PathBuf>, path: &Path) -> bool {
    path.extension()
        .map(|ext| ext == "nfo" && !known_files.contains(path))
        .unwrap_or(false)
}
