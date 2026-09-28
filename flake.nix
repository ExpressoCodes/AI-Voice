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

          # cmake and ninja are needed by whisper-rs-sys sub-builds, but we do
          # NOT want nixpkgs' cmake/ninja setup hooks to replace the cargo
          # build/install phases with cmake/ninja top-level phases.
          dontUseCmakeConfigure = true;
          dontUseNinjaBuild     = true;
          dontUseNinjaInstall   = true;
          dontUseNinjaCheck     = true;

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

            # TLS (openssl-sys)
            openssl

            # espeak-ng: system library used by espeak-rs-sys (see postPatch)
            espeak-ng
          ];

          # ORT_LIB_LOCATION tells ort-sys where to find libonnxruntime.so.
          # ORT_PREFER_DYNAMIC_LINK=1 forces dynamic linking; without it,
          # ort-sys tries static linking, which fails because nixpkgs only
          # ships shared libraries.  ORT_DYLIB_PATH is the runtime-load hint
          # used by the ort Rust crate itself.
          # LIBCLANG_PATH is required by whisper-rs bindgen.
          # ESPEAK_NG_DIR is read by the patched espeak-rs-sys build.rs (see
          # postPatch) to locate the system espeak-ng headers and library.
          env = {
            ORT_LIB_LOCATION        = "${pkgs.onnxruntime}/lib";
            ORT_PREFER_DYNAMIC_LINK = "1";
            ORT_DYLIB_PATH          = "${pkgs.onnxruntime}/lib/libonnxruntime.so";
            LIBCLANG_PATH           = "${pkgs.llvmPackages.libclang.lib}/lib";
            ESPEAK_NG_DIR           = "${pkgs.espeak-ng}";
          };

          # Two vendor crates need build-script patches so they don't attempt
          # network access inside the Nix sandbox.  Both .cargo-checksum.json
          # files have "files":{} so cargo does not verify individual file
          # hashes; no checksum update is needed after patching.
          #
          # 1. espeak-rs-sys 0.1.9: its bundled espeak-ng cmake build fails
          #    because vwl_en_us_nyc/a_raised is SPECTSQ2 format, which the
          #    bundled compiler rejects.  Replace the build script to link
          #    against the nixpkgs espeak-ng shared library instead.
          #
          # 2. jpreprocess-naist-jdic 0.12.0: its build script downloads a
          #    large Japanese dictionary from GitHub at compile time.  Replace
          #    it with a script that builds an equivalent (minimal) dictionary
          #    from synthetic data that requires no network access.
          postPatch = ''
            # ── patch 1: espeak-rs-sys ──────────────────────────────────────
            espeak_sys="$cargoDepsCopy/espeak-rs-sys-0.1.9"
            cat > "$espeak_sys/build.rs" << 'ESPEAK_EOF'
use std::env;
use std::path::PathBuf;

fn main() {
    let espeak_dir = env::var("ESPEAK_NG_DIR")
        .expect("ESPEAK_NG_DIR must be set to the espeak-ng installation prefix");

    let include_dir = format!("{}/include", espeak_dir);
    let lib_dir     = format!("{}/lib", espeak_dir);

    println!("cargo:rustc-link-search=native={}", lib_dir);
    println!("cargo:rustc-link-lib=dylib=espeak-ng");
    if cfg!(target_os = "linux") {
        println!("cargo:rustc-link-lib=dylib=stdc++");
    }

    let bindings = bindgen::Builder::default()
        .header(format!("{}/espeak-ng/speak_lib.h", include_dir))
        .clang_arg(format!("-I{}", include_dir))
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("Failed to generate bindings");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_dir.join("bindings.rs"))
        .expect("Failed to write bindings");
}
ESPEAK_EOF

            # ── patch 2: jpreprocess-naist-jdic ────────────────────────────
            naist_jdic="$cargoDepsCopy/jpreprocess-naist-jdic-0.12.0"
            cat > "$naist_jdic/build.rs" << 'NAIST_EOF'
use std::error::Error;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");

    #[cfg(feature = "naist-jdic")]
    build_dummy_dict()?;

    Ok(())
}

#[cfg(feature = "naist-jdic")]
fn build_dummy_dict() -> Result<(), Box<dyn Error>> {
    let build_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let input_dir = build_dir.join("naist-jdic-0.1.3");
    let output_dir = build_dir.join("naist-jdic");

    println!("cargo::rustc-env=LINDERA_WORKDIR={}", build_dir.display());

    if output_dir.is_dir() {
        return Ok(());
    }

    fs::create_dir_all(&input_dir)?;

    let mut char_def = fs::File::create(input_dir.join("char.def"))?;
    char_def.write_all(b"DEFAULT 0 1 0\n")?;

    let dummy_word = "\u{30c6}\u{30b9}\u{30c8},1343,1343,3195,\u{540d}\u{8a5e},\u{30b5}\u{5909}\u{63a5}\u{7d9a},*,*,*,*,\u{30c6}\u{30b9}\u{30c8},\u{30c6}\u{30b9}\u{30c8},\u{30c6}\u{30b9}\u{30c8},1/3,C1\n";
    let mut dict_csv = fs::File::create(input_dir.join("dummy_dict.csv"))?;
    dict_csv.write_all(dummy_word.as_bytes())?;

    fs::File::create(input_dir.join("unk.def"))?;

    let mut matrix_def = fs::File::create(input_dir.join("matrix.def"))?;
    matrix_def.write_all(b"0 1 0\n")?;

    let tmp_path = build_dir.join("tmp-output-naist-jdic");
    let _ = fs::remove_dir_all(&tmp_path);

    use lindera_dictionary::dictionary_builder::DictionaryBuilder;
    jpreprocess_dictionary::dictionary::to_dict::JPreprocessDictionaryBuilder::new()
        .build_dictionary(&input_dir, &tmp_path)?;

    let _ = fs::remove_dir_all(&output_dir);
    fs::rename(&tmp_path, &output_dir)?;

    Ok(())
}
NAIST_EOF
          '';

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
