// Records the target triple, which names this build in an update manifest.
fn main() {
    println!(
        "cargo:rustc-env=TRIAD_TARGET={}",
        std::env::var("TARGET").expect("cargo sets TARGET")
    );
    println!("cargo:rerun-if-changed=build.rs");
}
