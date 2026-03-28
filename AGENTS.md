# Repository Guidelines

## Project Structure & Module Organization
This repository is a Rust workspace (`Cargo.toml` at root) with crates under `packages/`:

- `packages/kk`: FLTK + MPV desktop player (main UI app), plus Lua helper scripts in `packages/kk/lua/`.
- `packages/kl`: CLI for scraping, tidying, and cache/server tooling.
- `packages/kr`: Shared core models and DB logic used by apps/tools.
- `packages/ks`: Sync/cache HTTP server for database distribution.
- `packages/kwa`: WebDAV library + CLI support.
- `packages/dirs`: Config/path utilities shared across crates.

Keep feature code inside its owning crate; only move shared logic into `kr`/`dirs` when reused.

## Build, Test, and Development Commands
- `cargo check --workspace`: fast compile validation for all crates.
- `cargo build --workspace`: full debug build of workspace binaries/libs.
- `cargo test --workspace`: run unit tests across all crates.
- `cargo run -p kk`: launch the GUI player.
- `cargo run -p kl -- <subcommand>`: run CLI flows (example: `cargo run -p kl -- tidy --help`).
- `cargo run -p ks -- -p 7070`: start sync server on port 7070.
- `cargo fmt --all` and `cargo clippy --workspace --all-targets -D warnings`: formatting and lint gate before PR.

## Coding Style & Naming Conventions
Use standard Rust style: 4-space indentation, `snake_case` for functions/modules, `PascalCase` for types/traits, and `SCREAMING_SNAKE_CASE` for constants. Prefer small modules and explicit error propagation (`anyhow::Result` where appropriate). Run `cargo fmt --all` before committing.

## Testing Guidelines
Use Rust’s built-in test framework (`#[test]`), with tests colocated in module files or `mod tests` blocks. Name tests by behavior, e.g., `parses_fc2_id_with_dash`. Add/adjust tests for parser, DB, and utility changes; at minimum run `cargo test --workspace` and `cargo check --workspace`.

## Commit & Pull Request Guidelines
Recent history follows Conventional Commit style, especially `feat:` (e.g., `feat: add ...`). Continue with `feat:`, `fix:`, `refactor:`, `chore:`, and keep subject lines imperative and scoped.

PRs should include:
- concise problem/solution summary,
- affected crates (example: `kk`, `kr`),
- linked issue(s) when available,
- screenshots/video for `kk` UI changes,
- verification notes listing commands run.

## Security & Configuration Tips
Use environment variables for sensitive config (`KK_WEBDAV_URL`, `KK_WEBDAV_USER`, `KK_WEBDAV_PASS`) and avoid committing secrets. `KK_SEARCH_PATH` is required for local media scanning in `kk`.
