{
  description = "VoiceChat — real-time AI voice assistant";

  inputs = {
    nixpkgs.url      = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url  = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs     = import nixpkgs { inherit system overlays; };

        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" ];
        };

        voicechat = pkgs.rustPlatform.buildRustPackage {
          pname   = "voicechat";
          version = "0.1.0";
          src     = ./.;

          # Path-patched kokorox lives under vendor/kokorox inside src,
          # so no allowBuiltinFetchGit is needed — it has no remote source.
          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = with pkgs; [
            pkg-config
            clang
            llvmPackages.libclang
            cmake
            ninja
            wrapGAppsHook4
          ];

          buildInputs = with pkgs; [
            # GUI
            gtk4
            libadwaita
            glib
            pango
            cairo
            gdk-pixbuf
            graphene

            # Audio
            alsa-lib
            pipewire

            # ML inference (ort/Silero VAD + Kokoro TTS)
            onnxruntime

            # Whisper CPU acceleration
            openblas
          ];

          # ORT_DYLIB_PATH must be set at build time so ort-sys can link the dylib.
          # LIBCLANG_PATH is required by whisper-rs bindgen.
          env = {
            ORT_DYLIB_PATH   = "${pkgs.onnxruntime}/lib/libonnxruntime.so";
            LIBCLANG_PATH    = "${pkgs.llvmPackages.libclang.lib}/lib";
          };

          # The release profile in Cargo.toml already sets strip = "symbols",
          # so the installed binary is lean without extra stripAllList.

          meta = with pkgs.lib; {
            description = "Real-time AI voice assistant using Whisper, Kokoro TTS, and Claude";
            license     = licenses.mit;
            platforms   = platforms.linux;
            mainProgram = "voicechat";
          };
        };
      in {
        packages.default = voicechat;

        apps.default = {
          type    = "app";
          program = "${voicechat}/bin/voicechat";
        };

        # Keep the dev shell for local hacking
        devShells.default = pkgs.mkShell {
          name = "voicechat";

          nativeBuildInputs = with pkgs; [
            rustToolchain
            pkg-config
            clang
            llvmPackages.libclang
            cmake
            ninja
          ];

          buildInputs = with pkgs; [
            gtk4
            libadwaita
            glib
            pango
            cairo
            gdk-pixbuf
            graphene
            pipewire
            alsa-lib
            onnxruntime
            espeak-ng
            openblas
            cacert
          ];

          shellHook = ''
            export LIBCLANG_PATH="${pkgs.llvmPackages.libclang.lib}/lib"
            export BINDGEN_EXTRA_CLANG_ARGS="-I${pkgs.glib.dev}/include/glib-2.0 -I${pkgs.glib.out}/lib/glib-2.0/include"
            export ORT_DYLIB_PATH="${pkgs.onnxruntime}/lib/libonnxruntime.so"
            export PIPEWIRE_RUNTIME_DIR="''${PIPEWIRE_RUNTIME_DIR:-/run/user/$(id -u)}"
            export PKG_CONFIG_PATH="${pkgs.openssl.dev}/lib/pkgconfig:$PKG_CONFIG_PATH"

            echo "voicechat dev shell ready"
            echo "Set ANTHROPIC_API_KEY before running"
            echo "Run: cargo build"
          '';
        };
      }
    ) // {
      nixosModules.default = { config, lib, pkgs, ... }: {
        options.programs.voicechat.enable =
          lib.mkEnableOption "VoiceChat AI voice assistant";

        config = lib.mkIf config.programs.voicechat.enable {
          environment.systemPackages =
            [ self.packages.${pkgs.system}.default ];
        };
      };
    };
}
