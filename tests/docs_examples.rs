use std::fs;
use std::path::Path;
use std::process::Command;

#[test]
fn build_writes_docs_under_out_dir() {
    let out_dir = Path::new(env!("TUISHOU_GENERATED_DOCS_DIR"));
    for (file, args) in [
        ("tuishou.bash", vec!["completions", "bash"]),
        ("tuishou.zsh", vec!["completions", "zsh"]),
        ("tuishou.fish", vec!["completions", "fish"]),
        ("tuishou.1", vec!["man"]),
    ] {
        let generated = fs::read_to_string(out_dir.join(file))
            .unwrap_or_else(|e| panic!("build did not generate {file} under OUT_DIR: {e}"));
        assert!(!generated.is_empty(), "generated {file} is empty");
        let from_cli = Command::new(env!("CARGO_BIN_EXE_tuishou"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(generated, String::from_utf8_lossy(&from_cli.stdout));
    }
    let source_dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("dist");
    assert!(
        !source_dist.exists(),
        "build must not write {} into the source tree",
        source_dist.display()
    );
}

#[test]
fn generated_docs_are_available_from_the_cli() {
    for shell in ["bash", "zsh", "fish"] {
        let out = Command::new(env!("CARGO_BIN_EXE_tuishou"))
            .args(["completions", shell])
            .output()
            .unwrap();
        assert!(out.status.success());
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            text.contains("tuishou"),
            "completion for {shell} is empty: {text}"
        );
    }
    let out = Command::new(env!("CARGO_BIN_EXE_tuishou"))
        .arg("man")
        .output()
        .unwrap();
    assert!(out.status.success());
    let man = String::from_utf8_lossy(&out.stdout);
    assert!(man.contains(".TH TUISHOU"));
    assert!(man.contains("self-update"));
}
