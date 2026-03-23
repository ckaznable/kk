# Project KK

A comprehensive Rust-based media management system consisting of a high-performance GUI video player and a metadata scraping/organizing CLI toolkit.

## Project Structure

This project is organized as a Rust workspace with the following members:

- **`packages/kk`**: The main GUI application. Built with [FLTK](https://fltk-rs.github.io/fltk-rs/) and [libmpv](https://mpv.io/). It provides a theater-like experience for browsing and playing local media. Now supports WebDAV streaming.
- **`packages/kl`**: A CLI utility for scraping movie metadata, organizing files (tidying), and fixing database inconsistencies. Supports local and WebDAV-based scraping.
- **`packages/kr`**: The core library containing shared data models (`Movie`, `Actor`) and database implementations for local and WebDAV metadata.
- **`packages/ks`**: A centralized web server that hosts `kl`'s scraping endpoints and serves/stores the JSON databases (`kr.json`, `kwa_db.json`) for syncing across machines.
- **`packages/kwa`**: A WebDAV client library and CLI tool for interacting with remote file servers.
- **`packages/dirs`**: Manages system paths, environment configuration, and `config.toml` parsing.

## Key Features

- **GUI Player (`kk`)**:
  - MPV-backed video playback with hardware acceleration.
  - Media browser with filtering by added time, random, favorites, or actors.
  - **WebDAV Mode**: Browse and stream remote videos directly via authenticated URLs.
  - Interactive timestamp markers with Lua script integration.
  - Keyboard-driven navigation (Vim-like shortcuts).
- **Metadata Scraping (`kl`)**:
  - Automatically identifies movie IDs (Jav, FC2, etc.) from filenames.
  - **Local Scraping**: Scrapes metadata, generates `.nfo` files, and organizes videos.
  - **WebDAV Scraping**: Scrapes remote files and stores metadata in a dedicated WebDAV database (`kwa_db.json`) without downloading the videos.
  - Scrapes metadata, posters, and thumbnails (cached locally).
- **Sync Server (`ks`)**:
  - Hosts `kr.json` and `kwa_db.json` via REST API (`GET`/`PUT /db/kr`, `GET`/`PUT /db/kwa`).
  - Proxies the `/cache` scraper endpoint from `kl`.
  - Enables `kk` to pull databases on startup and push back on exit.
- **WebDAV Support (`kwa`)**:
  - List remote directory contents and check path existence.
  - Generate authenticated stream URLs for MPV.

## Getting Started

### Prerequisites

- **Rust**: Latest stable version.
- **libmpv**: The player requires `libmpv` development libraries.
  - **Windows**: Place `libmpv-2.dll` in the `lib/` directory in the project root.
- **FLTK Dependencies**: Ensure you have the necessary libraries for FLTK (X11/Wayland headers on Linux).

### Environment Variables

- **`KK_SEARCH_PATH`**: (Required) The root directory where the application should scan for local media.
- **`KK_WEBDAV_URL`**, **`KK_WEBDAV_USER`**, **`KK_WEBDAV_PASS`**: Optional environment variables for the WebDAV CLI and scraper.

### Building and Running

```bash
# Set search path (Example for Windows PowerShell)
$env:KK_SEARCH_PATH = "D:\Media\Videos"
$env:KK_WEBDAV_URL = "http://example.com/dav"

# Run the GUI player
cargo run -p kk

# Scrape metadata from WebDAV (Uses ENV or DB config if flags are omitted)
cargo run -p kl -- webdav /videos

# Initial setup or override WebDAV info
cargo run -p kl -- webdav /videos --url http://example.com/dav -u user -p pass

# Scrape and organize local files
cargo run -p kl -- tidy --input ./new_videos --output D:\Media\Videos

# Test scrape a single ID without saving anything
cargo run -p kl -- test-scrape SSIS-123

# Start the ks sync server (default port 7070)
cargo run -p ks

# Start ks with custom port and data directory
cargo run -p ks -- -p 8080 -d /path/to/data
```

## Configuration and Persistence

- **Local Database**: `<config_local_dir>/kr.json`
- **WebDAV Database**: `<config_local_dir>/kwa_db.json`
- **Config File**: `<config_local_dir>/config.toml`
- **Thumbnails**: Cached in the user's local cache directory under `kk/thumbs`.

### config.toml

Optional configuration file. Currently supports:

```toml
[ks]
base_url = "http://your-server:7070"
```

When `ks.base_url` is set:
- **`kk`** pulls `kr.json` and `kwa_db.json` from `ks` on startup (overwriting local copies), and pushes them back on exit.
- **`kl`** pushes `kr.json` to `ks` after completing a `tidy` operation.
