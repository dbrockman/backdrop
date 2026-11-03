# backdrop
An app for browsing, organizing, and managing wallpapers.

## Installing with Nix

The flake builds Backdrop for x86_64 and aarch64 Linux. CI pushes builds to a
Cachix cache, so Nix downloads the binary instead of compiling it. Nix asks
whether to use the cache the first time; answer yes.

```sh
nix run github:dbrockman/backdrop            # try it
nix profile install github:dbrockman/backdrop
```

On NixOS, add the flake as an input and install the package:

```nix
{
  inputs.backdrop.url = "github:dbrockman/backdrop";

  outputs = { nixpkgs, backdrop, ... }: {
    nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
      modules = [
        ({ pkgs, ... }: {
          environment.systemPackages = [ backdrop.packages.${pkgs.system}.default ];
          nix.settings = {
            substituters = [ "https://dbrockman.cachix.org" ];
            trusted-public-keys = [ "dbrockman.cachix.org-1:oUXtudDZ0/g0qNzHvdMJSf93Orf8Fn+9NUJMUQJXP9I=" ];
          };
        })
      ];
    };
  };
}
```

There is also `overlays.default`, which adds `pkgs.backdrop`.

Don't make the input follow your own nixpkgs
(`inputs.backdrop.inputs.nixpkgs.follows`). The cached binaries are built
against this flake's locked nixpkgs. With a different one, Nix compiles the app
locally, which takes a while and needs a recent Rust.

Backdrop sets wallpapers through the `noctalia` command of the Noctalia shell,
which it expects to find on your `PATH`.

## Development

Backdrop is a Rust workspace built with [GPUI Kit](https://gpui-kit.com) 0.9.0.
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
