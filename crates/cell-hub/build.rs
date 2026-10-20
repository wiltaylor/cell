//! The web UI is embedded from `apps/hub-web/dist`. When it has not been
//! built, a placeholder page is embedded instead so `cargo build` works
//! without Node.
use std::path::Path;

fn main() {
    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/hub-web/dist");
    if !dist.join("index.html").exists() {
        std::fs::create_dir_all(&dist).ok();
        std::fs::write(
            dist.join("index.html"),
            "<!doctype html><title>Cell Hub</title><body style=\"font-family:sans-serif;background:#1b2838;color:#c7d5e0;padding:2rem\"><h1>Cell Hub</h1><p>The web UI was not built. Run <code>pnpm --dir apps/hub-web build</code> and rebuild the hub. The API is live under <code>/api</code>.</p>",
        )
        .ok();
    }
    println!("cargo:rerun-if-changed=../../apps/hub-web/dist");
}
