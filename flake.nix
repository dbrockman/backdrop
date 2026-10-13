{
  description = "Browse, organize, download, and manage wallpapers";

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
    in
    {
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
