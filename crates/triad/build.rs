//! With the `web` feature, embeds the built web app (`web/dist`) in the
//! binary: writes `$OUT_DIR/web_assets.rs`, a table of
//! `(path, content type, include_bytes!(…))`. Without the feature the table
//! is empty and nothing under `web/` is read, so a plain build needs no Node.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let mut table = String::from("pub static ASSETS: &[(&str, &str, &[u8])] = &[\n");
    println!("cargo::rerun-if-changed=build.rs");
    if std::env::var_os("CARGO_FEATURE_WEB").is_some() {
        let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest"));
        let dist = manifest.join("../../web/dist");
        println!("cargo::rerun-if-changed={}", dist.display());
        if !dist.join("index.html").is_file() {
            panic!(
                "\n\nthe `web` feature embeds the built web app, and {} has no index.html.\n\
                 Build it first:\n\n    pnpm -C web install --frozen-lockfile && pnpm -C web build\n\n\
                 or build triad without `--features web`.\n",
                dist.display()
            );
        }
        let mut files = Vec::new();
        walk(&dist, &mut files);
        files.sort();
        for file in files {
            println!("cargo::rerun-if-changed={}", file.display());
            let rel = file.strip_prefix(&dist).expect("under dist");
            let url = format!(
                "/{}",
                rel.components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/")
            );
            let abs = file.canonicalize().expect("canonical path");
            writeln!(
                table,
                "    ({url:?}, {:?}, include_bytes!({:?})),",
                content_type(&file),
                abs.display().to_string()
            )
            .unwrap();
        }
    }
    table.push_str("];\n");
    std::fs::write(out.join("web_assets.rs"), table).expect("writing web_assets.rs");
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("reading web/dist") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}
