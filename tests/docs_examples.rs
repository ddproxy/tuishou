use std::fs;
use std::path::Path;
use std::process::Command;

#[test]
fn build_writes_docs_into_dist() {
    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("dist");
    for (file, args) in [
        ("tuishou.bash", vec!["completions", "bash"]),
        ("tuishou.zsh", vec!["completions", "zsh"]),
        ("tuishou.fish", vec!["completions", "fish"]),
        ("tuishou.1", vec!["man"]),
    ] {
        let generated = fs::read_to_string(dist.join(file))
            .unwrap_or_else(|e| panic!("build did not generate dist/{file}: {e}"));
        assert!(!generated.is_empty(), "dist/{file} is empty");
        let from_cli = Command::new(env!("CARGO_BIN_EXE_tuishou"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(generated, String::from_utf8_lossy(&from_cli.stdout));
    }
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
