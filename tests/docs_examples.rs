use std::fs;
use std::path::Path;
use std::process::Command;

fn first_toml_block(doc: &str) -> String {
    let mut lines = doc.lines();
    while let Some(line) = lines.next() {
        if line.trim() == "```toml" {
            let mut block = String::new();
            for line in lines.by_ref() {
                if line.trim() == "```" {
                    return block;
                }
                block.push_str(line);
                block.push('\n');
            }
        }
    }
    panic!("no ```toml fenced block found in docs");
}

#[test]
fn documented_roll_inventory_drives_a_dry_run() {
    let doc = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/fleet-roll.md"))
        .unwrap();
    let example = first_toml_block(&doc);
    // Also make sure the example is valid TOML on its own.
    let parsed: toml::Value = toml::from_str(&example).unwrap();

    let td = tempfile::tempdir().unwrap();
    let inventory = td.path().join("inventory.toml");
    fs::write(&inventory, &example).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_tuishou"))
        .env("HOME", td.path())
        .env("XDG_STATE_HOME", td.path().join(".local/state"))
        .args(["roll", "--nodes"])
        .arg(&inventory)
        .args([
            "--component",
            "lu",
            "--version",
            "v0.5.0",
            "--dry-run",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let nodes = plan.as_array().unwrap();
    assert_eq!(nodes.len(), 2);
    assert_eq!(nodes[0]["node"], "node-1");
    assert_eq!(nodes[1]["node"], "node-2");
    assert_eq!(parsed["node"][0]["ssh"].as_str(), Some("deploy@10.0.0.11"));
}

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
