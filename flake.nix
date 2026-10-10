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

          # A font for the check phase. src/text.rs's measure_tests build a
          # cosmic-text FontSystem, and FontSystem::new() loads *system* fonts
          # through fontdb. With none loaded every stage of cosmic-text's fallback
          # chain comes up empty and shape.rs panics on its
          # expect("no default font found") -- three tests, in any sandbox.
          #
          # FONTCONFIG_FILE is the knob because of the fontdb dependency this
          # crate declares in Cargo.toml with features = ["fontconfig"], and the
          # note above it explains why: it is there to switch the feature on for
          # cosmic-text's transitive copy of fontdb, which cannot do it itself.
          # With that feature on, fontdb's load_system_fonts() takes its
          # load_fontconfig() branch and never looks at the directories it
          # otherwise hardcodes (/usr/share/fonts, $HOME/.fonts and friends) -- so
          # planting a font in HOME does nothing, which is worth knowing because it
          # is the obvious thing to try. load_fontconfig() reads FONTCONFIG_FILE
          # ahead of /etc/fonts, then walks each <dir> it finds, recursing, so one
          # generated conf pointing at a font package is enough.
          #
          # DejaVu named explicitly rather than left to makeFontsConf's default
          # (which happens to include dejavu-fonts-minimal): cosmic-text lists
          # "DejaVu Sans" in its own common_fallback(), so it is found by name
          # instead of by landing in the "any font that is not a forbidden
          # fallback" case at the end of the chain.
          #
          # No nativeCheckInputs needed with it -- the generated conf names the
          # font package by store path, so it arrives as part of that input's
          # closure.
          #
          # This also makes the measurements mean one thing. Run outside Nix the
          # tests pick up whatever is in ~/.local/share/fonts, which on the machine
          # these were written on is two display fonts from 2004 and 2024, so the
          # numbers moved with the host.
          preCheck = ''
            export FONTCONFIG_FILE=${pkgs.makeFontsConf { fontDirectories = [ pkgs.dejavu_fonts ]; }}
          '';

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