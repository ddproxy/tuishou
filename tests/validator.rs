use std::fs;
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

#[test]
fn dev_release_follows_ci_and_dispatches_the_release_workflow() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/dev-release.yml");
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("workflows: [CI]"));
    assert!(text.contains("github.event.workflow_run.conclusion == 'success'"));
    assert!(text.contains("gh workflow run release.yml --ref \"$TAG\""));
    assert!(text.contains("python3 scripts/set-release-version.py dev-tag"));
    let out = run(&path);
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn release_workflow_builds_both_musl_targets() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/release.yml");
    let text = fs::read_to_string(&path).unwrap();
    for target in ["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"] {
        assert!(
            text.contains(&format!("target: {target}")),
            "missing {target} in release matrix"
        );
    }
    assert!(text.contains("arch: x86_64"));
    assert!(text.contains("arch: aarch64"));
    assert!(text.contains("OUT=\"tuishou-${TAG}-${TARGET}\""));
    assert!(text.contains("tuishou-${{ github.ref_name }}-${{ matrix.target }}.tar.gz"));
    assert!(text.contains("workflow_dispatch:"));
    assert!(text.contains("'!v*-dev.*'"));
    assert!(text.contains("prune-list --keep 10"));
    assert!(text.contains("set-release-version.py set"));
    assert!(
        text.contains("AARCH64_MUSL_LINKER"),
        "cross-linker limit must be explicit"
    );
    assert!(
        text.contains("skipping aarch64 release target"),
        "unsupported cross-linking must be skipped, not guessed"
    );
    assert!(!text.contains("aarch64-linux-gnu-gcc"));
    let out = run(&path);
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn fuzz_workflow_runs_weekly_smoke_and_keeps_failures_only() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/fuzz.yml");
    let text = fs::read_to_string(&path).unwrap();
    for needle in [
        "workflow_dispatch:",
        "schedule:",
        "cron: '0 6 * * 0'",
        "rustup toolchain install nightly",
        "make fuzz-smoke",
        "if: failure()",
        "path: fuzz/artifacts/",
    ] {
        assert!(text.contains(needle), "missing {needle} in fuzz workflow");
    }
    let out = run(&path);
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}
