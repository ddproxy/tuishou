// Materialise generated docs into dist/ as part of the build. The files are
// derived from src/docs_static.rs (also used by the CLI) and are gitignored.
include!("src/docs_static.rs");

use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=src/docs_static.rs");
    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("dist");
    if let Err(err) = std::fs::create_dir_all(&dist) {
        println!("cargo:warning=could not create {}: {err}", dist.display());
        return;
    }
    for (name, body) in DIST_FILES {
        let path = dist.join(name);
        if let Err(err) = std::fs::write(&path, body) {
            println!("cargo:warning=could not write {}: {err}", path.display());
        }
    }
}
