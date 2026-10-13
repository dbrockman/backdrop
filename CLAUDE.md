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

GUI and CLI share the same settings and database, so the design must allow concurrent access from both processes (spec §8.3). Settings (`backdrop_core::settings`, TOML in the config dir) are changed only through `Settings::update`, which re-reads the file before applying a change and saves atomically. The GUI never saves after a failed load, so a malformed file isn't replaced with defaults.

## GPUI Kit

`gpui-kit` is pinned to `=0.7.1`, and `Cargo.lock` pins `gpui-component`, `gpui-base`, `gpui-kit-assets` and `gpui-component-macros` to 0.7.1 too, matching the skills in `.claude/skills/` (downloaded from GPUI Kit v0.7.1). If upgrading, update the skills alongside, and re-pin those crates after any `cargo update`.

- Use the `gpui-kit` skill for any GPUI code and the `gpui-kit-design-guides` skill before any UI/layout/copy work. Their Coding and Design Guides are normative.
- Depend only on `gpui-kit`; GPUI is `use gpui_kit::*;`, components are `gpui_kit::component::…`.
- Verify signatures in the 0.7.1 sources under `~/.cargo/registry/src/*/gpui-component-0.7.1` (and `gpui-pre-0.3.8` for GPUI itself) rather than guessing. Many Lucide icons aren't in the default bundle: `IconName` variants available by default are listed in `gpui-kit-assets-0.7.1/default-icons.txt`.

## Conventions

Commit messages are plain imperative sentences (e.g. "Add thumbnail cache").
