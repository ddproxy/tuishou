use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::thread;

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

struct FakeServer {
    base: String,
    handle: Option<thread::JoinHandle<()>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl FakeServer {
    fn start_with<F>(make_routes: F) -> Self
    where
        F: FnOnce(&str) -> Vec<(String, Vec<u8>, String)>,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{}", addr);
        let routes = make_routes(&base);
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop2 = stop.clone();
        let handle = thread::spawn(move || {
            while !stop2.load(std::sync::atomic::Ordering::Relaxed) {
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
                        let (status, body, ct) = routes
                            .iter()
                            .find(|(p, _, _)| p == path)
                            .map(|(_, b, ct)| ("200 OK", b.clone(), ct.clone()))
                            .unwrap_or_else(|| {
                                (
                                    "404 Not Found",
                                    b"not found".to_vec(),
                                    "text/plain".to_string(),
                                )
                            });
                        let resp = format!(
                            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: {ct}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(resp.as_bytes());
                        let _ = stream.write_all(&body);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        FakeServer {
            base,
            handle: Some(handle),
            stop,
        }
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn make_tar_gz(files: &[(&str, &[u8], Option<u32>)]) -> Vec<u8> {
    let mut tar_buf: Vec<u8> = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_buf);
        for (name, content, mode) in files {
            let mut header = tar::Header::new_gnu();
            header.set_path(name).unwrap();
            header.set_size(content.len() as u64);
            header.set_mode(mode.unwrap_or(0o755));
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

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    let out = h.finalize();
    out.iter().map(|b| format!("{:02x}", b)).collect()
}

fn tuishou_cmd() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tuishou"));
    // Debug-only fake keeps these HTTP-focused tests offline. PATH is empty to
    // prove that verification never needs an external CLI.
    cmd.env("TUISHOU_FAKE_ATTESTATION", "1")
        .env("PATH", "/nonexistent-tuishou-test-bin");
    cmd
}

fn setup_release(
    server_base: &str,
    repo: &str,
    tag: &str,
    asset_name: &str,
    archive: &[u8],
) -> Vec<(String, Vec<u8>, String)> {
    let checksum = format!("{}  {}\n", sha256_hex(archive), asset_name);
    let checksum_name = format!("{asset_name}.sha256");
    let release_json = serde_json::json!({
        "tag_name": tag,
        "body": format!("## {tag}\n\n- changes\n"),
        "assets": [
            {"name": asset_name, "browser_download_url": format!("{server_base}/dl/{asset_name}")},
            {"name": checksum_name, "browser_download_url": format!("{server_base}/dl/{checksum_name}")}
        ]
    }).to_string();
    let api_path_latest = format!("/repos/{repo}/releases/latest");
    let api_path_tag = format!("/repos/{repo}/releases/tags/{tag}");
    vec![
        (
            api_path_latest,
            release_json.clone().into_bytes(),
            "application/json".to_string(),
        ),
        (
            api_path_tag,
            release_json.into_bytes(),
            "application/json".to_string(),
        ),
        (
            format!("/dl/{asset_name}"),
            archive.to_vec(),
            "application/octet-stream".to_string(),
        ),
        (
            format!("/dl/{checksum_name}"),
            checksum.into_bytes(),
            "text/plain".to_string(),
        ),
    ]
}

fn base_env(cmd: &mut Command, server: &FakeServer, home: &Path) {
    cmd.env("TUISHOU_GITHUB_API_BASE", &server.base);
    cmd.env("TUISHOU_ALLOW_INSECURE_HTTP", "1");
    cmd.env("HOME", home);
    cmd.env("XDG_STATE_HOME", home.join(".local/state"));
    cmd.env_remove("GITHUB_TOKEN");
    cmd.env_remove("GH_TOKEN");
    // Keep verification offline and prove it does not need an external CLI.
    cmd.env("TUISHOU_FAKE_ATTESTATION", "1")
        .env("PATH", home.join("no-such-bin"));
}

#[test]
fn install_success_and_state_round_trip() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let prefix = td.path().join("prefix");
    let target = current_target();
    let archive = make_tar_gz(&[("lu", b"#!/bin/sh\necho lu\n", Some(0o755))]);
    let asset = format!("lu-v0.4.0-{target}.tar.gz");
    let server = FakeServer::start_with(|base| {
        setup_release(base, "ddproxy/tuishou-lu", "v0.4.0", &asset, &archive)
    });

    let mut cmd = tuishou_cmd();
    base_env(&mut cmd, &server, &home);
    cmd.args(["install", "lu", "--version", "v0.4.0", "--prefix"])
        .arg(&prefix)
        .arg("--allow-unattested");
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let bin = prefix.join("lu");
    assert!(bin.exists());
    let content = fs::read_to_string(&bin).unwrap();
    assert!(content.contains("echo lu"));

    let state_path = prefix.join(".tuishou.json");
    let state_text = fs::read_to_string(&state_path).unwrap();
    let state: serde_json::Value = serde_json::from_str(&state_text).unwrap();
    assert_eq!(state["components"]["lu"]["version"], "v0.4.0");
    assert_eq!(state["components"]["lu"]["target"], target);
    assert!(
        state["components"]["lu"]["checksum"]
            .as_str()
            .unwrap()
            .len()
            == 64
    );
}

fn current_target() -> String {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "x86_64-unknown-linux-musl".to_string(),
        ("linux", "aarch64") => "aarch64-unknown-linux-musl".to_string(),
        ("macos", "x86_64") => "x86_64-apple-darwin".to_string(),
        ("macos", "aarch64") => "aarch64-apple-darwin".to_string(),
        other => panic!("unsupported test target: {other:?}"),
    }
}

#[test]
fn wrong_checksum_fails() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let prefix = td.path().join("prefix");
    let target = current_target();
    let archive = make_tar_gz(&[("lu", b"good", Some(0o755))]);
    let asset = format!("lu-v0.4.0-{target}.tar.gz");
    let checksum_name = format!("{asset}.sha256");
    let server = FakeServer::start_with(|base| {
        let release_json = serde_json::json!({
            "tag_name": "v0.4.0",
            "body": "",
            "assets": [
                {"name": asset, "browser_download_url": format!("{base}/dl/{asset}")},
                {"name": checksum_name, "browser_download_url": format!("{base}/dl/{checksum_name}")}
            ]
        }).to_string();
        vec![
            (
                "/repos/ddproxy/tuishou-lu/releases/tags/v0.4.0".to_string(),
                release_json.into_bytes(),
                "application/json".to_string(),
            ),
            (
                format!("/dl/{asset}"),
                archive.clone(),
                "application/octet-stream".to_string(),
            ),
            (
                format!("/dl/{checksum_name}"),
                format!("{}\n", "0".repeat(64)).into_bytes(),
                "text/plain".to_string(),
            ),
        ]
    });

    let out = tuishou_cmd()
        .env("TUISHOU_GITHUB_API_BASE", &server.base)
        .env("TUISHOU_ALLOW_INSECURE_HTTP", "1")
        .env("HOME", &home)
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .args(["install", "lu", "--version", "v0.4.0", "--prefix"])
        .arg(&prefix)
        .arg("--allow-unattested")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(output.contains("checksum mismatch"), "output={output}");
    assert!(!prefix.join("lu").exists());
}

#[test]
fn truncated_download_fails_closed() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let prefix = td.path().join("prefix");
    let target = current_target();
    let archive = make_tar_gz(&[("lu", b"good", Some(0o755))]);
    let truncated = archive[..archive.len() / 2].to_vec();
    let asset = format!("lu-v0.4.0-{target}.tar.gz");
    let checksum_name = format!("{asset}.sha256");
    let server = FakeServer::start_with(|base| {
        let release_json = serde_json::json!({
            "tag_name": "v0.4.0", "body": "",
            "assets": [
                {"name": asset, "browser_download_url": format!("{base}/dl/{asset}")},
                {"name": checksum_name, "browser_download_url": format!("{base}/dl/{checksum_name}")}
            ]
        }).to_string();
        vec![
            (
                "/repos/ddproxy/tuishou-lu/releases/tags/v0.4.0".to_string(),
                release_json.into_bytes(),
                "application/json".to_string(),
            ),
            (
                format!("/dl/{asset}"),
                truncated.clone(),
                "application/octet-stream".to_string(),
            ),
            (
                format!("/dl/{checksum_name}"),
                format!("{}  {}\n", sha256_hex(&archive), asset).into_bytes(),
                "text/plain".to_string(),
            ),
        ]
    });
    let out = tuishou_cmd()
        .env("TUISHOU_GITHUB_API_BASE", &server.base)
        .env("TUISHOU_ALLOW_INSECURE_HTTP", "1")
        .env("HOME", &home)
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .args(["install", "lu", "--version", "v0.4.0", "--prefix"])
        .arg(&prefix)
        .arg("--allow-unattested")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(!prefix.join("lu").exists());
}

#[test]
fn oversize_download_fails() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let prefix = td.path().join("prefix");
    let target = current_target();
    let archive = make_tar_gz(&[("lu", &vec![b'a'; 2048], Some(0o755))]);
    let asset = format!("lu-v0.4.0-{target}.tar.gz");
    let checksum_name = format!("{asset}.sha256");
    let server = FakeServer::start_with(|base| {
        let release_json = serde_json::json!({
            "tag_name": "v0.4.0", "body": "",
            "assets": [
                {"name": asset, "browser_download_url": format!("{base}/dl/{asset}")},
                {"name": checksum_name, "browser_download_url": format!("{base}/dl/{checksum_name}")}
            ]
        }).to_string();
        vec![
            (
                "/repos/ddproxy/tuishou-lu/releases/tags/v0.4.0".to_string(),
                release_json.into_bytes(),
                "application/json".to_string(),
            ),
            (
                format!("/dl/{asset}"),
                archive.clone(),
                "application/octet-stream".to_string(),
            ),
            (
                format!("/dl/{checksum_name}"),
                format!("{}  {}\n", sha256_hex(&archive), asset).into_bytes(),
                "text/plain".to_string(),
            ),
        ]
    });
    let out = tuishou_cmd()
        .env("TUISHOU_GITHUB_API_BASE", &server.base)
        .env("TUISHOU_ALLOW_INSECURE_HTTP", "1")
        .env("TUISHOU_MAX_DOWNLOAD_BYTES", "10")
        .env("HOME", &home)
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .args(["install", "lu", "--version", "v0.4.0", "--prefix"])
        .arg(&prefix)
        .arg("--allow-unattested")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        output.contains("size limit")
            || output.contains("too large")
            || output.contains("exceeded"),
        "output={output}"
    );
    assert!(!prefix.join("lu").exists());
}

fn make_raw_tar_with_path(path: &str, content: &[u8]) -> Vec<u8> {
    // Minimal ustar header
    let mut header = [0u8; 512];
    let name_bytes = path.as_bytes();
    let n = name_bytes.len().min(100);
    header[..n].copy_from_slice(&name_bytes[..n]);
    // mode
    write_octal(&mut header[100..108], 0o755);
    // uid/gid
    write_octal(&mut header[108..116], 0);
    write_octal(&mut header[116..124], 0);
    write_octal(&mut header[124..136], content.len() as u64);
    write_octal(&mut header[136..148], 0);
    // checksum field spaces
    for b in &mut header[148..156] {
        *b = b' ';
    }
    header[156] = b'0'; // regular file
                        // magic
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    let csum: u32 = header.iter().map(|b| *b as u32).sum();
    write_octal(&mut header[148..156], csum as u64);
    header[155] = 0;
    header[154] = b' ';
    let mut tar_buf = Vec::new();
    tar_buf.extend_from_slice(&header);
    tar_buf.extend_from_slice(content);
    let pad = (512 - (content.len() % 512)) % 512;
    tar_buf.extend(std::iter::repeat_n(0u8, pad));
    tar_buf.extend(std::iter::repeat_n(0u8, 1024)); // end
    let mut gz = Vec::new();
    let mut enc = flate2::write::GzEncoder::new(&mut gz, flate2::Compression::default());
    enc.write_all(&tar_buf).unwrap();
    enc.finish().unwrap();
    gz
}

fn write_octal(field: &mut [u8], val: u64) {
    let s = format!("{:o}", val);
    let bytes = s.as_bytes();
    let start = field.len().saturating_sub(1 + bytes.len());
    for b in &mut field[..start] {
        *b = b'0';
    }
    for (i, b) in bytes.iter().enumerate() {
        if start + i < field.len() - 1 {
            field[start + i] = *b;
        }
    }
    if let Some(last) = field.last_mut() {
        *last = 0;
    }
}

fn make_tar_gz_with_symlink(name: &str, target: &str) -> Vec<u8> {
    let mut tar_buf: Vec<u8> = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_buf);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        builder.append_link(&mut header, name, target).unwrap();
        builder.finish().unwrap();
    }
    let mut gz = Vec::new();
    let mut enc = flate2::write::GzEncoder::new(&mut gz, flate2::Compression::default());
    enc.write_all(&tar_buf).unwrap();
    enc.finish().unwrap();
    gz
}

#[test]
fn archive_path_traversal_rejected() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let prefix = td.path().join("prefix");
    let target = current_target();
    let archive = make_raw_tar_with_path("../lu", b"evil");
    let asset = format!("lu-v0.4.0-{target}.tar.gz");
    let checksum_name = format!("{asset}.sha256");
    let server = FakeServer::start_with(|base| {
        let release_json = serde_json::json!({"tag_name":"v0.4.0","body":"","assets":[
            {"name":asset,"browser_download_url":format!("{base}/dl/{asset}")},
            {"name":checksum_name,"browser_download_url":format!("{base}/dl/{checksum_name}")}
        ]})
        .to_string();
        vec![
            (
                "/repos/ddproxy/tuishou-lu/releases/tags/v0.4.0".to_string(),
                release_json.into_bytes(),
                "application/json".to_string(),
            ),
            (
                format!("/dl/{asset}"),
                archive.clone(),
                "application/octet-stream".to_string(),
            ),
            (
                format!("/dl/{checksum_name}"),
                format!("{}  {}\n", sha256_hex(&archive), asset).into_bytes(),
                "text/plain".to_string(),
            ),
        ]
    });
    let out = tuishou_cmd()
        .env("TUISHOU_GITHUB_API_BASE", &server.base)
        .env("TUISHOU_ALLOW_INSECURE_HTTP", "1")
        .env("HOME", &home)
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .args(["install", "lu", "--version", "v0.4.0", "--prefix"])
        .arg(&prefix)
        .arg("--allow-unattested")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        output.contains("path traversal") || output.contains("archive"),
        "output={output}"
    );
}

#[test]
fn archive_symlink_rejected() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let prefix = td.path().join("prefix");
    let target = current_target();
    let archive = make_tar_gz_with_symlink("lu", "/etc/passwd");
    let asset = format!("lu-v0.4.0-{target}.tar.gz");
    let checksum_name = format!("{asset}.sha256");
    let server = FakeServer::start_with(|base| {
        let release_json = serde_json::json!({"tag_name":"v0.4.0","body":"","assets":[
            {"name":asset,"browser_download_url":format!("{base}/dl/{asset}")},
            {"name":checksum_name,"browser_download_url":format!("{base}/dl/{checksum_name}")}
        ]})
        .to_string();
        vec![
            (
                "/repos/ddproxy/tuishou-lu/releases/tags/v0.4.0".to_string(),
                release_json.into_bytes(),
                "application/json".to_string(),
            ),
            (
                format!("/dl/{asset}"),
                archive.clone(),
                "application/octet-stream".to_string(),
            ),
            (
                format!("/dl/{checksum_name}"),
                format!("{}  {}\n", sha256_hex(&archive), asset).into_bytes(),
                "text/plain".to_string(),
            ),
        ]
    });
    let out = tuishou_cmd()
        .env("TUISHOU_GITHUB_API_BASE", &server.base)
        .env("TUISHOU_ALLOW_INSECURE_HTTP", "1")
        .env("HOME", &home)
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .args(["install", "lu", "--version", "v0.4.0", "--prefix"])
        .arg(&prefix)
        .arg("--allow-unattested")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        output.contains("symlink") || output.contains("link") || output.contains("archive"),
        "output={output}"
    );
}

#[test]
fn prev_retention_and_rollback() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let prefix = td.path().join("prefix");
    let target = current_target();
    let a1 = make_tar_gz(&[("lu", b"v1", Some(0o755))]);
    let a2 = make_tar_gz(&[("lu", b"v2", Some(0o755))]);
    let asset1 = format!("lu-v0.4.0-{target}.tar.gz");
    let asset2 = format!("lu-v0.5.0-{target}.tar.gz");
    let server = FakeServer::start_with(|base| {
        let mut routes = Vec::new();
        for (tag, asset, archive) in [
            ("v0.4.0", asset1.clone(), a1.clone()),
            ("v0.5.0", asset2.clone(), a2.clone()),
        ] {
            let checksum_name = format!("{asset}.sha256");
            let rel = serde_json::json!({"tag_name":tag,"body":"","assets":[
                {"name":asset,"browser_download_url":format!("{base}/dl/{asset}")},
                {"name":checksum_name,"browser_download_url":format!("{base}/dl/{checksum_name}")}
            ]})
            .to_string();
            routes.push((
                format!("/repos/ddproxy/tuishou-lu/releases/tags/{tag}"),
                rel.into_bytes(),
                "application/json".to_string(),
            ));
            routes.push((
                format!("/dl/{asset}"),
                archive.clone(),
                "application/octet-stream".to_string(),
            ));
            routes.push((
                format!("/dl/{checksum_name}"),
                format!("{}  {}\n", sha256_hex(&archive), asset).into_bytes(),
                "text/plain".to_string(),
            ));
        }
        // latest points to v0.5.0
        let latest = serde_json::json!({"tag_name":"v0.5.0","body":"","assets":[
            {"name":asset2,"browser_download_url":format!("{base}/dl/{asset2}")},
            {"name":format!("{asset2}.sha256"),"browser_download_url":format!("{base}/dl/{asset2}.sha256")}
        ]}).to_string();
        routes.push((
            "/repos/ddproxy/tuishou-lu/releases/latest".to_string(),
            latest.into_bytes(),
            "application/json".to_string(),
        ));
        routes
    });

    let base_env = |cmd: &mut Command| {
        cmd.env("TUISHOU_GITHUB_API_BASE", &server.base)
            .env("TUISHOU_ALLOW_INSECURE_HTTP", "1")
            .env("HOME", &home)
            .env("XDG_STATE_HOME", home.join(".local/state"))
            .env_remove("GITHUB_TOKEN")
            .env_remove("GH_TOKEN");
    };

    let mut cmd = tuishou_cmd();
    base_env(&mut cmd);
    let out = cmd
        .args(["install", "lu", "--version", "v0.4.0", "--prefix"])
        .arg(&prefix)
        .arg("--allow-unattested")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let mut cmd = tuishou_cmd();
    base_env(&mut cmd);
    let out = cmd
        .args(["install", "lu", "--version", "v0.5.0", "--prefix"])
        .arg(&prefix)
        .arg("--allow-unattested")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(fs::read(prefix.join("lu")).unwrap(), b"v2");
    assert_eq!(fs::read(prefix.join("lu.prev")).unwrap(), b"v1");

    let mut cmd = tuishou_cmd();
    base_env(&mut cmd);
    let out = cmd
        .args(["rollback", "lu", "--prefix"])
        .arg(&prefix)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(fs::read(prefix.join("lu")).unwrap(), b"v1");
}

#[test]
fn update_check_exit_codes() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let prefix = td.path().join("prefix");
    let target = current_target();
    let a1 = make_tar_gz(&[("lu", b"v1", Some(0o755))]);
    let a2 = make_tar_gz(&[("lu", b"v2", Some(0o755))]);
    let asset1 = format!("lu-v0.4.0-{target}.tar.gz");
    let asset2 = format!("lu-v0.5.0-{target}.tar.gz");
    let server = FakeServer::start_with(|base| {
        let mut routes = Vec::new();
        for (tag, asset, archive) in [
            ("v0.4.0", asset1.clone(), a1.clone()),
            ("v0.5.0", asset2.clone(), a2.clone()),
        ] {
            let checksum_name = format!("{asset}.sha256");
            let rel = serde_json::json!({"tag_name":tag,"body":"## " ,"assets":[
                {"name":asset,"browser_download_url":format!("{base}/dl/{asset}")},
                {"name":checksum_name,"browser_download_url":format!("{base}/dl/{checksum_name}")}
            ]})
            .to_string();
            routes.push((
                format!("/repos/ddproxy/tuishou-lu/releases/tags/{tag}"),
                rel.into_bytes(),
                "application/json".to_string(),
            ));
            routes.push((
                format!("/dl/{asset}"),
                archive.clone(),
                "application/octet-stream".to_string(),
            ));
            routes.push((
                format!("/dl/{checksum_name}"),
                format!("{}  {}\n", sha256_hex(&archive), asset).into_bytes(),
                "text/plain".to_string(),
            ));
        }
        let latest = serde_json::json!({"tag_name":"v0.5.0","body":"## v0.5.0\n- x","assets":[
            {"name":asset2,"browser_download_url":format!("{base}/dl/{asset2}")},
            {"name":format!("{asset2}.sha256"),"browser_download_url":format!("{base}/dl/{asset2}.sha256")}
        ]}).to_string();
        routes.push((
            "/repos/ddproxy/tuishou-lu/releases/latest".to_string(),
            latest.into_bytes(),
            "application/json".to_string(),
        ));
        routes
    });
    let base_env = |cmd: &mut Command| {
        cmd.env("TUISHOU_GITHUB_API_BASE", &server.base)
            .env("TUISHOU_ALLOW_INSECURE_HTTP", "1")
            .env("HOME", &home)
            .env("XDG_STATE_HOME", home.join(".local/state"))
            .env_remove("GITHUB_TOKEN")
            .env_remove("GH_TOKEN");
    };

    let mut cmd = tuishou_cmd();
    base_env(&mut cmd);
    let out = cmd
        .args(["install", "lu", "--version", "v0.4.0", "--prefix"])
        .arg(&prefix)
        .arg("--allow-unattested")
        .output()
        .unwrap();
    assert!(out.status.success());

    let mut cmd = tuishou_cmd();
    base_env(&mut cmd);
    let out = cmd
        .args(["update", "lu", "--check", "--prefix"])
        .arg(&prefix)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(10));

    // update for real
    let mut cmd = tuishou_cmd();
    base_env(&mut cmd);
    let out = cmd
        .args(["update", "lu", "--prefix"])
        .arg(&prefix)
        .arg("--allow-unattested")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let mut cmd = tuishou_cmd();
    base_env(&mut cmd);
    let out = cmd
        .args(["update", "lu", "--check", "--prefix"])
        .arg(&prefix)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn major_version_refusal() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let prefix = td.path().join("prefix");
    let target = current_target();
    let a1 = make_tar_gz(&[("lu", b"v1", Some(0o755))]);
    let a2 = make_tar_gz(&[("lu", b"v2", Some(0o755))]);
    let asset1 = format!("lu-v0.4.0-{target}.tar.gz");
    let asset2 = format!("lu-v1.0.0-{target}.tar.gz");
    let server = FakeServer::start_with(|base| {
        let mut routes = Vec::new();
        for (tag, asset, archive) in [
            ("v0.4.0", asset1.clone(), a1.clone()),
            ("v1.0.0", asset2.clone(), a2.clone()),
        ] {
            let checksum_name = format!("{asset}.sha256");
            let rel = serde_json::json!({"tag_name":tag,"body":"","assets":[
                {"name":asset,"browser_download_url":format!("{base}/dl/{asset}")},
                {"name":checksum_name,"browser_download_url":format!("{base}/dl/{checksum_name}")}
            ]})
            .to_string();
            routes.push((
                format!("/repos/ddproxy/tuishou-lu/releases/tags/{tag}"),
                rel.into_bytes(),
                "application/json".to_string(),
            ));
            routes.push((
                format!("/dl/{asset}"),
                archive.clone(),
                "application/octet-stream".to_string(),
            ));
            routes.push((
                format!("/dl/{checksum_name}"),
                format!("{}  {}\n", sha256_hex(&archive), asset).into_bytes(),
                "text/plain".to_string(),
            ));
        }
        let latest = serde_json::json!({"tag_name":"v1.0.0","body":"","assets":[
            {"name":asset2,"browser_download_url":format!("{base}/dl/{asset2}")},
            {"name":format!("{asset2}.sha256"),"browser_download_url":format!("{base}/dl/{asset2}.sha256")}
        ]}).to_string();
        routes.push((
            "/repos/ddproxy/tuishou-lu/releases/latest".to_string(),
            latest.into_bytes(),
            "application/json".to_string(),
        ));
        routes
    });
    let base_env = |cmd: &mut Command| {
        cmd.env("TUISHOU_GITHUB_API_BASE", &server.base)
            .env("TUISHOU_ALLOW_INSECURE_HTTP", "1")
            .env("HOME", &home)
            .env("XDG_STATE_HOME", home.join(".local/state"))
            .env_remove("GITHUB_TOKEN")
            .env_remove("GH_TOKEN");
    };

    let mut cmd = tuishou_cmd();
    base_env(&mut cmd);
    let out = cmd
        .args(["install", "lu", "--version", "v0.4.0", "--prefix"])
        .arg(&prefix)
        .arg("--allow-unattested")
        .output()
        .unwrap();
    assert!(out.status.success());

    let mut cmd = tuishou_cmd();
    base_env(&mut cmd);
    let out = cmd
        .args(["update", "lu", "--prefix"])
        .arg(&prefix)
        .arg("--allow-unattested")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(output.contains("--allow-major"), "output={output}");
}

#[test]
fn unknown_config_key_errors() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(&home).unwrap();
    fs::write(td.path().join("tuishou.toml"), "unknown_key = 1\n").unwrap();
    let out = tuishou_cmd()
        .current_dir(td.path())
        .env("HOME", &home)
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .args(["status"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown") || stderr.contains("unknown_key"),
        "stderr={stderr}"
    );
}

fn route_for_default(
    base: &str,
    repo: &str,
    tag: &str,
    asset: &str,
    archive: &[u8],
) -> Vec<(String, Vec<u8>, String)> {
    setup_release(base, repo, tag, asset, archive)
}

#[test]
fn bare_update_check_reports_every_default_in_table_order() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let prefix = td.path().join("prefix");
    let target = current_target();
    let tag = "v0.5.0";
    let specs = [
        (
            "ddproxy/reiki",
            format!("reiki-{tag}-{target}.tar.gz"),
            make_tar_gz(&[("reiki", b"reiki", Some(0o755))]),
        ),
        (
            "ddproxy/tuishou-lu",
            format!("lu-{tag}-{target}.tar.gz"),
            make_tar_gz(&[("lu", b"lu", Some(0o755))]),
        ),
        (
            "ddproxy/tuishou",
            format!("tuishou-{tag}-{target}.tar.gz"),
            make_tar_gz(&[("tuishou", b"tuishou", Some(0o755))]),
        ),
    ];
    let server = FakeServer::start_with(|base| {
        let mut routes = Vec::new();
        for (repo, asset, archive) in &specs {
            routes.extend(route_for_default(base, repo, tag, asset, archive));
        }
        routes
    });

    let mut cmd = tuishou_cmd();
    base_env(&mut cmd, &server, &home);
    let out = cmd
        .args(["update", "--check", "--prefix"])
        .arg(&prefix)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "stdout={stdout}");
    let reiki = stdout
        .find("reiki: not installed (latest v0.5.0)")
        .expect(&stdout);
    let lu = stdout
        .find("lu: not installed (latest v0.5.0)")
        .expect(&stdout);
    let tuishou = stdout
        .find("tuishou: not installed (latest v0.5.0)")
        .expect(&stdout);
    assert!(reiki < lu && lu < tuishou, "order wrong: {stdout}");
}

#[test]
fn unknown_component_is_a_usage_error_listing_known_names() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(&home).unwrap();

    let mut cmd = tuishou_cmd();
    cmd.env("HOME", &home)
        .env("XDG_STATE_HOME", home.join(".local/state"));
    let out = cmd.args(["update", "nope", "--check"]).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(2), "{combined}");
    assert!(combined.contains("unknown component 'nope'"), "{combined}");
    for name in ["reiki", "lu", "tuishou"] {
        assert!(combined.contains(name), "{combined}");
    }
}

#[test]
fn check_reports_all_components_when_one_evaluation_fails() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let prefix = td.path().join("prefix");
    let target = current_target();
    let tag = "v0.5.0";
    let specs = [
        (
            "ddproxy/tuishou-lu",
            format!("lu-{tag}-{target}.tar.gz"),
            make_tar_gz(&[("lu", b"lu", Some(0o755))]),
        ),
        (
            "ddproxy/tuishou",
            format!("tuishou-{tag}-{target}.tar.gz"),
            make_tar_gz(&[("tuishou", b"tuishou", Some(0o755))]),
        ),
    ];
    // No routes for reiki: its evaluation fails while the others still report.
    let server = FakeServer::start_with(|base| {
        let mut routes = Vec::new();
        for (repo, asset, archive) in &specs {
            routes.extend(route_for_default(base, repo, tag, asset, archive));
        }
        routes
    });

    let mut cmd = tuishou_cmd();
    base_env(&mut cmd, &server, &home);
    let out = cmd
        .args(["update", "--check", "--prefix"])
        .arg(&prefix)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "stdout={stdout}");
    assert!(stdout.contains("reiki: failed ("), "{stdout}");
    assert!(
        stdout.contains("lu: not installed (latest v0.5.0)"),
        "{stdout}"
    );
    assert!(
        stdout.contains("tuishou: not installed (latest v0.5.0)"),
        "{stdout}"
    );
}

#[test]
fn explicit_update_installs_a_not_installed_component() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let prefix = td.path().join("prefix");
    let target = current_target();
    let tag = "v0.5.0";
    let asset = format!("lu-{tag}-{target}.tar.gz");
    let archive = make_tar_gz(&[("lu", b"new lu", Some(0o755))]);
    let server = FakeServer::start_with(|base| {
        setup_release(base, "ddproxy/tuishou-lu", tag, &asset, &archive)
    });

    let mut cmd = tuishou_cmd();
    base_env(&mut cmd, &server, &home);
    let out = cmd
        .args(["update", "lu", "--prefix"])
        .arg(&prefix)
        .arg("--allow-unattested")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "stdout={stdout}");
    assert!(stdout.contains("lu: updated - -> v0.5.0"), "{stdout}");
    assert_eq!(fs::read(prefix.join("lu")).unwrap(), b"new lu");
}

#[test]
fn install_all_reports_missing_binaries_without_failing() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let prefix = td.path().join("prefix");
    let target = current_target();
    let tag = "v0.5.0";
    let specs = [
        (
            "ddproxy/reiki",
            format!("reiki-{tag}-{target}.tar.gz"),
            make_tar_gz(&[("other", b"x", Some(0o755))]),
        ),
        (
            "ddproxy/tuishou-lu",
            format!("lu-{tag}-{target}.tar.gz"),
            make_tar_gz(&[("lu", b"lu", Some(0o755))]),
        ),
        (
            "ddproxy/tuishou",
            format!("tuishou-{tag}-{target}.tar.gz"),
            make_tar_gz(&[("tuishou", b"tuishou", Some(0o755))]),
        ),
    ];
    let server = FakeServer::start_with(|base| {
        let mut routes = Vec::new();
        for (repo, asset, archive) in &specs {
            routes.extend(route_for_default(base, repo, tag, asset, archive));
        }
        routes
    });

    let mut cmd = tuishou_cmd();
    base_env(&mut cmd, &server, &home);
    let out = cmd
        .args(["install", "all", "--prefix"])
        .arg(&prefix)
        .arg("--allow-unattested")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stdout={stdout} stderr={stderr}");
    assert!(
        stdout.contains("reiki: not in this release (archive lacks: reiki)"),
        "{stdout}"
    );
    assert!(stdout.contains("lu: updated - -> v0.5.0"), "{stdout}");
    assert!(stdout.contains("tuishou: updated - -> v0.5.0"), "{stdout}");
    assert!(!prefix.join("reiki").exists());
    assert!(prefix.join("lu").exists());
    assert!(prefix.join("tuishou").exists());
}

#[test]
fn group_writable_config_is_refused() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let config = td.path().join("tuishou.toml");
    fs::write(&config, "[auto_update]\nenabled = true\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&config, fs::Permissions::from_mode(0o664)).unwrap();
    }

    let mut cmd = tuishou_cmd();
    let out = cmd
        .current_dir(td.path())
        .env("HOME", &home)
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .args(["status"])
        .output()
        .unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "{combined}");
    assert!(combined.contains("untrusted config"), "{combined}");
}

#[test]
fn component_repo_override_is_refused() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(&home).unwrap();
    fs::write(
        td.path().join("tuishou.toml"),
        "[[component]]\nname = \"lu\"\nrepo = \"evil/example\"\n",
    )
    .unwrap();

    let mut cmd = tuishou_cmd();
    let out = cmd
        .current_dir(td.path())
        .env("HOME", &home)
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .args(["status"])
        .output()
        .unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "{combined}");
    assert!(combined.contains("cannot override repo"), "{combined}");
}

#[test]
fn removed_roll_command_is_unknown() {
    let out = tuishou_cmd()
        .args(["roll", "--nodes", "nodes.toml", "--dry-run"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("unrecognized subcommand"), "output={text}");
}

#[test]
fn removed_roll_is_absent_from_completions_and_man_page() {
    for args in [vec!["completions", "bash"], vec!["man"]] {
        let out = tuishou_cmd().args(args).output().unwrap();
        assert!(out.status.success());
        let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
        assert!(!text.contains("roll --nodes"), "output={text}");
        assert!(!text.contains("--nodes"), "output={text}");
        assert!(!text.contains("over ssh"), "output={text}");
    }
}

#[test]
fn removed_roll_config_sections_are_unknown_keys() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    for text in [
        "[roll]\nenabled = true\n",
        "[ssh]\nport = 22\n",
        "[inventory]\npath = \"nodes.toml\"\n",
        "[[node]]\nname = \"n1\"\nssh = \"n1\"\nunit = \"lu-agent.service\"\n",
    ] {
        let td = tempfile::tempdir().unwrap();
        let home = td.path().join("home");
        fs::create_dir_all(&home).unwrap();
        fs::write(td.path().join("tuishou.toml"), text).unwrap();
        let out = tuishou_cmd()
            .current_dir(td.path())
            .env("HOME", &home)
            .env("XDG_STATE_HOME", home.join(".local/state"))
            .args(["status"])
            .output()
            .unwrap();
        assert!(!out.status.success(), "config={text}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("unknown config key"),
            "config={text} stderr={stderr}"
        );
    }
}
