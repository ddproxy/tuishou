// Materialise generated docs under OUT_DIR as part of the build. The files are
// derived from src/docs_static.rs (also used by the CLI). Nothing is written
// into the source tree.
include!("src/docs_static.rs");

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=src/docs_static.rs");
    let out_dir = PathBuf::from(
        std::env::var_os("OUT_DIR").expect("cargo must set OUT_DIR for build scripts"),
    );
    println!(
        "cargo:rustc-env=TUISHOU_GENERATED_DOCS_DIR={}",
        out_dir.display()
    );
    for (name, body) in DIST_FILES {
        let path = out_dir.join(name);
        if let Err(err) = std::fs::write(&path, body) {
            println!("cargo:warning=could not write {}: {err}", path.display());
        }
    }
}
