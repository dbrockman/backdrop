# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Backdrop is a Linux desktop app for browsing, organizing, downloading (Wallhaven), and setting wallpapers, built with Rust and GPUI Kit. It is both a GUI and a CLI. `spec.md` is the product specification: requirements, the phased roadmap (§10), and open design decisions (§11). Read the relevant section before implementing a feature, and don't silently resolve an open decision.

## Commands

Development happens on NixOS. GPUI needs Wayland/X11, xkbcommon, Vulkan and fontconfig from `flake.nix`. `just` recipes wrap themselves in `nix develop` automatically; plain `cargo` must run inside `nix develop` (it fails on `yeslogic-fontconfig-sys` otherwise).

```sh
just                 # list recipes
just run [args]      # no args opens the GUI; e.g. `just run paths` runs a CLI command
just test [args]     # cargo test --workspace
just lint            # cargo fmt --check + clippy -D warnings (both must be clean)
just check           # lint + test
```

Single test: `just test -p backdrop-core files_live_in_their_xdg_directories`, or `nix develop -c cargo test -p backdrop cli::tests`.

The dev shell sets `LD_LIBRARY_PATH` because GPUI `dlopen`s Wayland, Vulkan, xkbcommon and fontconfig; keep `runtimeLibraries` in `flake.nix` in sync with what GPUI loads.

## Architecture

Two crates; dependencies point one way: **backdrop → backdrop-core**.

- `crates/backdrop-core`: all behavior shared by GUI and CLI (library indexing, SQLite metadata, thumbnails, providers, desktop backends, git, settings). It must not depend on GPUI. `paths::AppPaths` resolves the XDG locations: settings in the config dir, the SQLite index in the data dir, thumbnails in the cache dir. Nothing Backdrop generates is written inside the user's wallpaper library, except `.backdrop/`, which holds everything that cannot be regenerated offline (user metadata, provider snapshots). SQLite is only a rebuildable index of the library (spec §5).
- `crates/backdrop`: the `backdrop` binary. `main.rs` parses clap `Args`: no command (or `gui`) calls `gui::run()`, anything else goes to `cli::run()`, which never starts GPUI. `gui/mod.rs` bootstraps GPUI Kit (init, actions/keybindings, window options with app id `backdrop`); `gui/workspace.rs` is the root view.

Library scanning: `discovery` walks the library (skipping hidden entries and symlinks) and reads only the headers of new or changed files, judged by size and mtime; `index::Index::scan` applies the result to SQLite in one transaction. Headers are read with the `image` crate's decoders, and dimensions are stored upright (EXIF quarter turns swap width and height) so orientation matches what users see. Sorting and filtering (`query::ImageOrder`, `query::ImageFilter`) run in Rust, not SQL, so the GUI and CLI share one implementation. The index is derived data: a `SCHEMA_VERSION` bump deletes and rebuilds the database instead of migrating, and the index covers one library root (scanning another root clears it). Images under folders that couldn't be read are kept, never treated as removed.

`library::Library` is what the GUI and CLI use: it pairs the index with `metadata::LibraryMetadata` (`.backdrop/metadata.toml`, the source of truth for user data) and reconciles them on every scan, following renamed and moved images by size and blake3 hash (spec §5.3). Hashes are computed only on demand and cached in the index's `hash` column. Metadata changes go through `LibraryMetadata::update`, which re-reads before writing; reconciliation skips any record another process changed in the meantime. Removing many records at once is returned as `unconfirmed_removals` for the front end to confirm, then `Library::remove_missing`.

Thumbnails (`thumbnails::Thumbnails`) are JPEGs in the XDG cache, named after the image's absolute path, size and mtime (no content read), upright per EXIF. `gui/library_grid.rs` renders the library as rows in a `uniform_list`; the column count comes from the width measured by a `canvas` and is frozen per frame in `render` (the row renderer must use `self.columns`, never recompute it, or the row count and rows disagree). Thumbnails are generated in display order in background batches, and tiles render `img(...)` only once their thumbnail exists, because GPUI caches load failures. `gui/thumbnail_cache.rs` is an LRU `ImageCache` that bounds decoded thumbnails in memory. The grid owns the images, selection and favorites; `gui/image_detail.rs` (the full-window view, opened by the grid's `OpenImage` event) only reads the grid and moves its selection, so the workspace just swaps which view it renders and closing returns to the same image. Grid and detail behavior have UI integration tests (`#[gpui_kit::test]`, `gpui-kit` `test-support` dev feature); key bindings live in `gui::bind_keys` so tests use the real ones.

Wallpapers are read and set through `desktop::WallpaperBackend`; the one implementation, `desktop::Noctalia`, shells out to `noctalia msg wallpaper-get`/`wallpaper-set` (spec §4.3). `desktop::detect()` runs a real `noctalia` command, so the GUI takes the detector as a parameter of `Workspace::new` and tests pass `|| None` or an in-memory fake; never let a test reach the real shell, since `wallpaper-set` changes the user's desktop. Noctalia can't report changes, so the workspace re-reads the current wallpaper when its window becomes active.

GUI and CLI share the same settings and database, so the design must allow concurrent access from both processes (spec §8.3). Settings (`backdrop_core::settings`, TOML in the config dir) are changed only through `Settings::update`, which re-reads the file before applying a change and saves atomically. The GUI never saves after a failed load, so a malformed file isn't replaced with defaults.

## GPUI Kit

`gpui-kit` is pinned to `=0.7.1`, and `Cargo.lock` pins `gpui-component`, `gpui-base`, `gpui-kit-assets` and `gpui-component-macros` to 0.7.1 too, matching the skills in `.claude/skills/` (downloaded from GPUI Kit v0.7.1). If upgrading, update the skills alongside, and re-pin those crates after any `cargo update`.

- Use the `gpui-kit` skill for any GPUI code and the `gpui-kit-design-guides` skill before any UI/layout/copy work. Their Coding and Design Guides are normative.
- Depend only on `gpui-kit`; GPUI is `use gpui_kit::*;`, components are `gpui_kit::component::…`.
- Verify signatures in the 0.7.1 sources under `~/.cargo/registry/src/*/gpui-component-0.7.1` (and `gpui-pre-0.3.8` for GPUI itself) rather than guessing. Many Lucide icons aren't in the default bundle: `IconName` variants available by default are listed in `gpui-kit-assets-0.7.1/default-icons.txt`.

## Conventions

Commit messages are plain imperative sentences (e.g. "Add thumbnail cache").
