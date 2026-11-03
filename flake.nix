{
  description = "Browse, organize, download, and manage wallpapers";

  # Prebuilt binaries, pushed by CI for each commit on main. Nix asks before
  # using them; see the README for adding the cache to a NixOS configuration.
  nixConfig = {
    extra-substituters = [ "https://dbrockman.cachix.org" ];
    extra-trusted-public-keys = [ "dbrockman.cachix.org-1:oUXtudDZ0/g0qNzHvdMJSf93Orf8Fn+9NUJMUQJXP9I=" ];
  };

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { nixpkgs, ... }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});

      # Loaded at runtime by GPUI (windowing, input, GPU, fonts).
      runtimeLibraries = pkgs: with pkgs; [
        wayland
        libxkbcommon
        vulkan-loader
        libxcb
        libx11
        fontconfig
        freetype
      ];

      package = pkgs:
        let
          inherit (pkgs) lib;
          cargoToml = lib.importTOML ./Cargo.toml;
        in
        pkgs.rustPlatform.buildRustPackage {
          pname = "backdrop";
          inherit (cargoToml.workspace.package) version;

          # Only what Cargo reads (crates/ includes the embedded themes), so
          # edits to docs or CI don't change the derivation and miss the cache.
          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [ ./Cargo.toml ./Cargo.lock ./crates ];
          };
          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = [ pkgs.pkg-config pkgs.resvg ];
          buildInputs = runtimeLibraries pkgs;

          # default-members is only the binary's crate; test backdrop-core too.
          cargoTestFlags = [ "--workspace" ];

          # GPUI dlopens most of these, which an installed binary can only find
          # through its RPATH (the dev shell uses LD_LIBRARY_PATH instead).
          postFixup = ''
            patchelf --add-rpath ${lib.makeLibraryPath (runtimeLibraries pkgs)} $out/bin/backdrop
          '';

          # The window's app_id is "backdrop", so the desktop file must be
          # named after it for Wayland compositors to match the two. The icon
          # goes into the hicolor theme as SVG and at the standard PNG sizes.
          postInstall = ''
            install -Dm644 ${./packaging/backdrop.desktop} $out/share/applications/backdrop.desktop
            install -Dm644 ${./packaging/backdrop.svg} $out/share/icons/hicolor/scalable/apps/backdrop.svg
            for size in 16 24 32 48 64 128 256 512; do
              dir=$out/share/icons/hicolor/''${size}x''${size}/apps
              mkdir -p $dir
              resvg -w $size -h $size ${./packaging/backdrop.svg} $dir/backdrop.png
            done
          '';

          meta = {
            description = "Browse, organize, download, and set wallpapers";
            homepage = "https://github.com/dbrockman/backdrop";
            license = lib.licenses.mit;
            platforms = systems;
            mainProgram = "backdrop";
          };
        };
    in
    {
      packages = forAllSystems (pkgs: {
        default = package pkgs;
      });

      overlays.default = final: _prev: {
        backdrop = package final;
      };

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [ cargo rustc clippy rustfmt rust-analyzer pkg-config just ];
          buildInputs = runtimeLibraries pkgs;
          env = {
            RUST_BACKTRACE = "1";
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (runtimeLibraries pkgs);
          };
        };
      });
    };
}
