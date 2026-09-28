fn main() {
    // Tell cargo to rerun if these env vars change
    println!("cargo:rerun-if-env-changed=ORT_DYLIB_PATH");
    println!("cargo:rerun-if-env-changed=WHISPER_DONT_GENERATE_WRAPPER");
    println!("cargo:rerun-if-env-changed=LIBCLANG_PATH");
}
