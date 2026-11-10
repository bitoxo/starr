// rust-embed needs the asset folder to exist at compile time. When the web UI
// has not been built yet (fresh clone, Rust-only work), create an empty one so
// the server still compiles and serves a placeholder page.
fn main() {
    let dist = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/dist");
    if !dist.exists() {
        let _ = std::fs::create_dir_all(&dist);
    }
    println!("cargo:rerun-if-changed=../../web/dist");
}
