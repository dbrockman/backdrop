# backdrop
An app for browsing, organizing, and managing wallpapers.

## Development

Backdrop is a Rust workspace built with [GPUI Kit](https://gpui-kit.com) 0.7.1.
GPUI needs Wayland/X11, xkbcommon, Vulkan, and fontconfig, which `flake.nix`
provides. `just` recipes enter the dev shell automatically; plain `cargo` must
run inside `nix develop`.

```sh
just               # list recipes
just run           # open the GUI
just run paths     # run a CLI command instead
just check         # fmt check, clippy, tests
```

- `crates/backdrop-core`: library, metadata, provider, and desktop logic, with no GUI dependency.
- `crates/backdrop`: the `backdrop` binary. With no command it opens the GUI (`src/gui`); with a command it runs the CLI (`src/cli.rs`) without starting GPUI.
