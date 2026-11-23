use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

struct FakeServer {
    base: String,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl FakeServer {
    fn start_with<F>(make_routes: F) -> Self
    where
        F: FnOnce(&str) -> Vec<(String, Vec<u8>)>,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let routes = make_routes(&base);
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let handle = thread::spawn(move || {
            while !stop2.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut buf = [0u8; 16384];
                        let n = stream.read(&mut buf).unwrap_or(0);
                        let req = String::from_utf8_lossy(&buf[..n]);
                        let path = req
                            .lines()
                            .next()
                            .and_then(|l| l.split_whitespace().nth(1))
                            .unwrap_or("/");
                        let (status, body) = routes
                            .iter()
                            .find(|(p, _)| p == path)
                            .map(|(_, b)| ("200 OK", b.clone()))
                            .unwrap_or(("404 Not Found", b"not found".to_vec()));
                        let header = format!(
                            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(header.as_bytes());
                        let _ = stream.write_all(&body);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            base,
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().iter().map(|b| format!("{:02x}", b)).collect()
}

fn tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut tar_buf = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_buf);
        for (name, content) in files {
            let mut header = tar::Header::new_gnu();
            header.set_path(name).unwrap();
            header.set_size(content.len() as u64);
            header.set_mode(0o755);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            builder.append(&header, *content).unwrap();
        }
        builder.finish().unwrap();
    }
    let mut gz = Vec::new();
    let mut enc = flate2::write::GzEncoder::new(&mut gz, flate2::Compression::default());
    enc.write_all(&tar_buf).unwrap();
    enc.finish().unwrap();
    gz
}

fn target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        other => panic!("unsupported test target: {other:?}"),
    }
}

fn release_routes(base: &str, archive: &[u8], checksum: &str, tag: &str) -> Vec<(String, Vec<u8>)> {
    let asset = format!("tuishou-{tag}-{}.tar.gz", target());
    let checksum_name = format!("{asset}.sha256");
    let json = serde_json::json!({
        "target_commitish":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","tag_name": tag,
        "body": format!("## {tag}\n- self update\n"),
        "assets": [
            {"name": asset, "browser_download_url": format!("{base}/dl/{asset}")},
            {"name": checksum_name, "browser_download_url": format!("{base}/dl/{checksum_name}")}
        ]
    })
    .to_string();
    vec![
        (
            format!("/repos/ddproxy/tuishou/releases/tags/{tag}"),
            json.into_bytes(),
        ),
        (format!("/dl/{asset}"), archive.to_vec()),
        (format!("/dl/{checksum_name}"), checksum.as_bytes().to_vec()),
    ]
}

fn env_for(cmd: &mut Command, server: &FakeServer, home: &Path) {
    cmd.env("TUISHOU_GITHUB_API_BASE", &server.base)
        .env("TUISHOU_ALLOW_INSECURE_HTTP", "1")
        .env("HOME", home)
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        // Use the debug-only fake verifier with no external CLI on PATH.
        .env("TUISHOU_FAKE_ATTESTATION", "1")
        .env("PATH", "/nonexistent-tuishou-test-bin");
}

fn copy_binary_to(dir: &Path) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let dest = dir.join("tuishou");
    fs::copy(env!("CARGO_BIN_EXE_tuishou"), &dest).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).unwrap();
    }
    dest
}

#[test]
fn self_update_replaces_running_binary_and_keeps_prev() {
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let bin_dir = td.path().join("bin");
    let binary = copy_binary_to(&bin_dir);
    let original = fs::read(&binary).unwrap();

    let replacement = b"#!/bin/sh\necho 'tuishou 9.9.9'\n";
    let archive = tar_gz(&[("tuishou", replacement)]);
    let checksum = format!(
        "{}  tuishou-v0.2.0-{}.tar.gz\n",
        sha256_hex(&archive),
        target()
    );
    let server = FakeServer::start_with(|base| release_routes(base, &archive, &checksum, "v0.2.0"));

    let mut cmd = Command::new(&binary);
    env_for(&mut cmd, &server, &home);
    // No --prefix: self-update must target the directory of the running binary.
    cmd.args(["self-update", "--version", "v0.2.0", "--allow-unattested"]);
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert_eq!(fs::read(&binary).unwrap(), replacement);
    let prev = bin_dir.join("tuishou.prev");
    assert_eq!(fs::read(&prev).unwrap(), original);

    let version = Command::new(&binary).arg("--version").output().unwrap();
    assert!(String::from_utf8_lossy(&version.stdout).contains("9.9.9"));

    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(bin_dir.join(".tuishou.json")).unwrap()).unwrap();
    assert_eq!(state["components"]["tuishou"]["version"], "v0.2.0");
}

#[test]
fn self_update_bad_checksum_leaves_binary_in_place() {
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let install_dir = td.path().join("prefix");
    let binary = copy_binary_to(&install_dir);
    let original = fs::read(&binary).unwrap();

    let archive = tar_gz(&[("tuishou", b"#!/bin/sh\necho 'tuishou 9.9.9'\n")]);
    let checksum = format!("{}  tuishou-v0.2.0-{}.tar.gz\n", "0".repeat(64), target());
    let server = FakeServer::start_with(|base| release_routes(base, &archive, &checksum, "v0.2.0"));

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tuishou"));
    env_for(&mut cmd, &server, &home);
    cmd.args(["self-update", "--version", "v0.2.0", "--prefix"])
        .arg(&install_dir)
        .arg("--allow-unattested");
    let out = cmd.output().unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("checksum mismatch"), "stderr={stderr}");
    assert_eq!(fs::read(&binary).unwrap(), original);
    assert!(!install_dir.join("tuishou.prev").exists());
}

#[test]
fn self_update_smoke_failure_leaves_binary_in_place() {
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let install_dir = td.path().join("prefix");
    let binary = copy_binary_to(&install_dir);
    let original = fs::read(&binary).unwrap();

    let archive = tar_gz(&[("tuishou", b"#!/bin/sh\nexit 1\n")]);
    let checksum = format!(
        "{}  tuishou-v0.2.0-{}.tar.gz\n",
        sha256_hex(&archive),
        target()
    );
    let server = FakeServer::start_with(|base| release_routes(base, &archive, &checksum, "v0.2.0"));

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tuishou"));
    env_for(&mut cmd, &server, &home);
    cmd.args(["self-update", "--version", "v0.2.0", "--prefix"])
        .arg(&install_dir)
        .arg("--allow-unattested");
    let out = cmd.output().unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("smoke check failed"), "stderr={stderr}");
    assert_eq!(fs::read(&binary).unwrap(), original);
    assert!(!install_dir.join("tuishou.prev").exists());
    assert!(!install_dir.join(".tuishou.json").exists());
}

#[test]
fn unattested_archive_never_executes_release_code() {
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let install_dir = td.path().join("prefix");
    let binary = copy_binary_to(&install_dir);
    let marker = td.path().join("release-code-executed");
    let replacement = format!(
        "#!/bin/sh\ntouch {}\necho 'tuishou 9.9.9'\n",
        marker.display()
    );
    let archive = tar_gz(&[("tuishou", replacement.as_bytes())]);
    let checksum = format!(
        "{}  tuishou-v0.2.0-{}.tar.gz\n",
        sha256_hex(&archive),
        target()
    );
    let server = FakeServer::start_with(|base| release_routes(base, &archive, &checksum, "v0.2.0"));

    let mut cmd = Command::new(&binary);
    env_for(&mut cmd, &server, &home);
    cmd.env("TUISHOU_FAKE_ATTESTATION_FAIL", "1");
    cmd.args(["self-update", "--version", "v0.2.0", "--prefix"])
        .arg(&install_dir)
        .arg("--allow-unattested");
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("staged checks are skipped"),
        "stderr={stderr}"
    );
    assert!(!marker.exists(), "unverified release code was executed");
    assert_eq!(fs::read(&binary).unwrap(), replacement.as_bytes());
}

#[test]
fn self_update_dry_run_does_not_write() {
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let install_dir = td.path().join("prefix");
    let binary = copy_binary_to(&install_dir);
    let original = fs::read(&binary).unwrap();

    let archive = tar_gz(&[("tuishou", b"#!/bin/sh\necho hi\n")]);
    let checksum = format!(
        "{}  tuishou-v0.2.0-{}.tar.gz\n",
        sha256_hex(&archive),
        target()
    );
    let server = FakeServer::start_with(|base| release_routes(base, &archive, &checksum, "v0.2.0"));

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tuishou"));
    env_for(&mut cmd, &server, &home);
    cmd.args(["self-update", "--version", "v0.2.0", "--prefix"])
        .arg(&install_dir)
        .args(["--allow-unattested", "--dry-run"]);
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(fs::read(&binary).unwrap(), original);
    assert!(!install_dir.join("tuishou.prev").exists());
}
