{
  description = "Inlyne - A GPU powered, browserless markdown viewer";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };

        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" ];
        };

        buildInputs = with pkgs; [
          # Core dependencies
          fontconfig
          freetype
          
          # Wayland dependencies
          wayland
          wayland-protocols
          libxkbcommon
          
          # X11 dependencies
          libx11
          libxcursor
          libxrandr
          libxi
          libxext
          
          # Graphics/GPU dependencies
          vulkan-loader
          libGL
          
          # Additional dependencies that might be needed
          openssl
          pkg-config
          cmake
          python3
        ];

        nativeBuildInputs = with pkgs; [
          rustToolchain
          pkg-config
          cmake
          python3
          makeWrapper
        ];

      in
      {
        packages.default = pkgs.rustPlatform.buildRustPackage rec {
          pname = "inlyne";
          version = "0.5.0";
          
          src = ./.;
          
          cargoLock = {
            lockFile = ./Cargo.lock;
          };
          
          inherit buildInputs nativeBuildInputs;
          
          # Set feature flags (both wayland and x11 are default)
          buildFeatures = [ "wayland" "x11" ];
          
          # Runtime dependencies for GPU acceleration
          postInstall = ''
            wrapProgram $out/bin/inlyne \
              --prefix LD_LIBRARY_PATH : "${pkgs.lib.makeLibraryPath [
                pkgs.vulkan-loader
                pkgs.libGL
                pkgs.wayland
                pkgs.libxkbcommon
              ]}"

            # Desktop entry, so inlyne shows up in launchers and can be set as
            # the handler for markdown files. `Exec` is rewritten to the
            # wrapped binary's absolute path: launchers do not necessarily
            # share the shell's PATH, and the bare name would otherwise find
            # an unwrapped binary (or none at all).
            install -Dm644 assets/inlyne.desktop \
              $out/share/applications/inlyne.desktop
            substituteInPlace $out/share/applications/inlyne.desktop \
              --replace-fail 'Exec=inlyne' "Exec=$out/bin/inlyne"
          '';
          
          meta = with pkgs.lib; {
            description = "A GPU powered yet browserless tool to help you quickly view markdown files";
            homepage = "https://github.com/Inlyne-Project/inlyne";
            license = licenses.mit;
            platforms = platforms.linux;
            mainProgram = "inlyne";
          };
        };
        
        devShells.default = pkgs.mkShell {
          inherit buildInputs nativeBuildInputs;
          
          shellHook = ''
            echo "Inlyne development environment"
            echo "Rust version: $(rustc --version)"
            echo ""
            echo "Build with: cargo build --release"
            echo "Run with: cargo run -- <markdown-file>"
            echo ""
            echo "To build the Nix package: nix build"
            echo "To run directly: nix run . -- <markdown-file>"
          '';
          
          # Set environment variables for better GPU support
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath buildInputs;
          PKG_CONFIG_PATH = pkgs.lib.makeSearchPath "lib/pkgconfig" buildInputs;
          
          # Vulkan/GPU environment variables
          VK_LAYER_PATH = "${pkgs.vulkan-validation-layers}/share/vulkan/explicit_layer.d";

          # Deliberately no VK_ICD_FILENAMES: `vulkan-loader` ships no ICDs (they
          # come from the graphics driver, e.g. mesa), so pointing at it left the
          # loader with zero drivers. Unset, the loader discovers the host's ICDs
          # under /run/opengl-driver/share/vulkan/icd.d, which is also what keeps
          # the driver matched to the running kernel.
        };
        
        apps.default = flake-utils.lib.mkApp {
          drv = self.packages.${system}.default;
        };
      });
}