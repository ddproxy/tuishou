use std::path::{Path, PathBuf};
use std::process::Command;

fn validator() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/validate-workflows.sh")
}

fn run(path: &Path) -> std::process::Output {
    Command::new("bash")
        .arg(validator())
        .arg(path)
        .output()
        .expect("failed to run workflow validator")
}

#[test]
fn validator_accepts_valid_fixtures() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/fixtures/workflows-valid");
    let out = run(&dir);
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn validator_rejects_every_negative_fixture() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/fixtures/workflows-invalid");
    let fixtures: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "yml"))
        .collect();
    assert!(!fixtures.is_empty(), "no negative fixtures found");
    for fixture in fixtures {
        let out = run(&fixture);
        assert!(
            !out.status.success(),
            "validator accepted negative fixture {}",
            fixture.display()
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("invalid uses: pin"),
            "unexpected output for {}: {stderr}",
            fixture.display()
        );
    }
}

#[test]
fn validator_rejects_out_of_range_sha_lengths() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/fixtures/workflows-invalid");
    for name in ["short-sha.yml", "long-sha.yml"] {
        let out = run(&dir.join(name));
        assert!(!out.status.success(), "{name} was accepted");
    }
}
