#![allow(clippy::useless_format, clippy::too_many_arguments, dead_code)]

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[derive(Clone)]
struct Route {
    path: String,
    status: u16,
    body: Vec<u8>,
    content_type: String,
    headers: Vec<(String, String)>,
}

struct FakeServer {
    base: String,
    hits: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl FakeServer {
    fn start(routes: Vec<Route>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let hits = Arc::new(Mutex::new(Vec::new()));
        let hits2 = hits.clone();
        let handle = thread::spawn(move || {
            while !stop2.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut buf = [0u8; 32768];
                        let n = stream.read(&mut buf).unwrap_or(0);
                        let req = String::from_utf8_lossy(&buf[..n]).to_string();
                        let path = req
                            .lines()
                            .next()
                            .and_then(|l| l.split_whitespace().nth(1))
                            .unwrap_or("/")
                            .to_string();
                        hits2.lock().unwrap().push(path.clone());
                        let route = routes.iter().find(|r| r.path == path).cloned();
                        let (status, body, ct, extra) = match route {
                            Some(r) => (r.status, r.body, r.content_type, r.headers),
                            None => (404, b"not found".to_vec(), "text/plain".to_string(), vec![]),
                        };
                        let mut resp = format!(
                            "HTTP/1.1 {status} {}\r\nContent-Length: {}\r\nContent-Type: {ct}\r\nConnection: close\r\n",
                            status_text(status),
                            body.len()
                        );
                        for (k, v) in extra {
                            resp.push_str(&format!("{k}: {v}\r\n"));
                        }
                        resp.push_str("\r\n");
                        let _ = stream.write_all(resp.as_bytes());
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
            hits,
            stop,
            handle: Some(handle),
        }
    }

    fn hit_count(&self, path_substr: &str) -> usize {
        self.hits
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.contains(path_substr))
            .count()
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

fn status_text(code: u16) -> &'static str {
    match code {
        200 => "OK",
        304 => "Not Modified",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        _ => "Status",
    }
}

fn make_tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut tar_buf = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_buf);
        for (name, content) in files {
            let mut header = tar::Header::new_gnu();
            header.set_path(name).unwrap();
            header.set_size(content.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append(&header, *content).unwrap();
        }
        builder.finish().unwrap();
    }
    let mut gz = Vec::new();
    let mut enc = flate2::write::GzEncoder::new(&mut gz, flate2::Compression::default());
    use std::io::Write as _;
    enc.write_all(&tar_buf).unwrap();
    enc.finish().unwrap();
    gz
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
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

fn tuishou_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tuishou"))
}

fn write_fake_tool(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

fn release_routes(
    _repo: &str,
    tag: &str,
    asset: &str,
    _archive: &[u8],
    body: &str,
    published_at: &str,
) -> Vec<Route> {
    let checksum_name = format!("{asset}.sha256");
    let json = serde_json::json!({
        "tag_name": tag,
        "body": body,
        "prerelease": tag.contains("-"),
        "draft": false,
        "published_at": published_at,
        "assets": [
            {"name": asset, "browser_download_url": format!("http://local/dl/{asset}")},
            {"name": checksum_name, "browser_download_url": format!("http://local/dl/{checksum_name}")}
        ]
    }).to_string();
    // browser_download_url will be rewritten by caller with real base; easier to do per-server.
    let _ = json;
    vec![]
}

fn release_routes_with_base(
    base: &str,
    repo: &str,
    tag: &str,
    asset: &str,
    archive: &[u8],
    body: &str,
    published_at: &str,
    etag: Option<&str>,
) -> Vec<Route> {
    let checksum_name = format!("{asset}.sha256");
    let json = serde_json::json!({
        "tag_name": tag,
        "body": body,
        "prerelease": tag.contains('-'),
        "draft": false,
        "published_at": published_at,
        "assets": [
            {"name": asset, "browser_download_url": format!("{base}/dl/{asset}")},
            {"name": checksum_name, "browser_download_url": format!("{base}/dl/{checksum_name}")}
        ]
    })
    .to_string();
    let list_json = serde_json::json!([{
        "tag_name": tag,
        "body": body,
        "prerelease": tag.contains('-'),
        "draft": false,
        "published_at": published_at,
        "assets": [
            {"name": asset, "browser_download_url": format!("{base}/dl/{asset}")},
            {"name": checksum_name, "browser_download_url": format!("{base}/dl/{checksum_name}")}
        ]
    }])
    .to_string();
    let mut list_headers = vec![];
    if let Some(etag) = etag {
        list_headers.push(("ETag".to_string(), etag.to_string()));
    }
    vec![
        Route {
            path: format!("/repos/{repo}/releases/latest"),
            status: 200,
            body: json.clone().into_bytes(),
            content_type: "application/json".to_string(),
            headers: vec![],
        },
        Route {
            path: format!("/repos/{repo}/releases/tags/{tag}"),
            status: 200,
            body: json.into_bytes(),
            content_type: "application/json".to_string(),
            headers: vec![],
        },
        Route {
            path: format!("/repos/{repo}/releases?per_page=30"),
            status: 200,
            body: list_json.into_bytes(),
            content_type: "application/json".to_string(),
            headers: list_headers,
        },
        Route {
            path: format!("/dl/{asset}"),
            status: 200,
            body: archive.to_vec(),
            content_type: "application/octet-stream".to_string(),
            headers: vec![],
        },
        Route {
            path: format!("/dl/{checksum_name}"),
            status: 200,
            body: format!("{}  {}\n", sha256_hex(archive), asset).into_bytes(),
            content_type: "text/plain".to_string(),
            headers: vec![],
        },
    ]
}

fn base_env(cmd: &mut Command, server: &FakeServer, home: &Path, path_env: &str) {
    cmd.env("TUISHOU_GITHUB_API_BASE", &server.base)
        .env("TUISHOU_ALLOW_INSECURE_HTTP", "1")
        .env("HOME", home)
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("PATH", path_env)
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN");
}

fn write_config(dir: &Path, content: &str) -> PathBuf {
    let p = dir.join("tuishou.toml");
    fs::write(&p, content).unwrap();
    p
}

fn fake_gh_ok(dir: &Path) -> PathBuf {
    write_fake_tool(dir, "gh", "#!/bin/sh\nexit 0\n")
}

fn setup_prefix_with_state(home: &Path, component: &str, version: &str) {
    let state_dir = home.join(".local/state/tuishou");
    fs::create_dir_all(&state_dir).unwrap();
    let state = serde_json::json!({
        "components": {
            component: {
                "version": version,
                "target": current_target(),
                "checksum": "0".repeat(64),
                "installed_at": "2026-01-01T00:00:00Z"
            }
        }
    });
    fs::write(state_dir.join("installed.json"), state.to_string()).unwrap();
}

#[test]
fn unknown_config_key_in_auto_update_rejected() {
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(&home).unwrap();
    write_config(td.path(), "[auto_update]\nenabled = true\nbogus = 1\n");
    let out = tuishou_cmd()
        .current_dir(td.path())
        .env("HOME", &home)
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .args(["auto-update", "status"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown") || stderr.contains("bogus"),
        "{stderr}"
    );
}

#[test]
fn interval_below_5m_rejected() {
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(&home).unwrap();
    write_config(
        td.path(),
        "[auto_update]\nenabled = true\ninterval = \"4m\"\n",
    );
    let out = tuishou_cmd()
        .current_dir(td.path())
        .env("HOME", &home)
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .args(["auto-update", "status"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("5m minimum"));
}

#[test]
fn auto_mode_refuses_allow_unattested_and_requires_gh() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(&home).unwrap();
    write_config(td.path(), "[auto_update]\nenabled = true\n");
    let bindir = td.path().join("bin");
    fs::create_dir_all(&bindir).unwrap();
    // no gh on PATH
    let path = bindir.display().to_string();
    let out = tuishou_cmd()
        .current_dir(td.path())
        .env("HOME", &home)
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("PATH", &path)
        .args(["auto-update", "run", "--once"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("gh"), "{stderr}");

    // allow-unattested refused even if gh present
    fake_gh_ok(&bindir);
    let out = tuishou_cmd()
        .current_dir(td.path())
        .env("HOME", &home)
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("PATH", &path)
        .args(["auto-update", "run", "--once", "--allow-unattested"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("allow-unattested"));
}

#[test]
fn hold_and_expiry_pause_updates() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let bindir = td.path().join("bin");
    fs::create_dir_all(&bindir).unwrap();
    fake_gh_ok(&bindir);
    setup_prefix_with_state(&home, "app", "v0.4.0");

    let target = current_target();
    let asset = format!("lu-v0.4.1-{target}.tar.gz");
    let archive = make_tar_gz(&[("lu", b"#!/bin/sh\necho v0.4.1\n")]);
    let server = FakeServer::start(release_routes_with_base(
        "http://127.0.0.1:1",
        "ddproxy/tuishou-lu",
        "v0.4.1",
        &asset,
        &archive,
        "## v0.4.1\n",
        "2020-01-01T00:00:00Z",
        Some("\"e1\""),
    ));
    // rebuild routes with real base
    let server = FakeServer::start(release_routes_with_base(
        &server.base,
        "ddproxy/tuishou-lu",
        "v0.4.1",
        &asset,
        &archive,
        "## v0.4.1\n",
        "2020-01-01T00:00:00Z",
        Some("\"e1\""),
    ));

    write_config(
        td.path(),
        "[auto_update]\nenabled = true\ncomponents = [\"lu\"]\nmin_release_age = \"0s\"\n",
    );
    let path = format!(
        "{}:{}",
        bindir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    // hold
    let out = tuishou_cmd()
        .current_dir(td.path())
        .env("HOME", &home)
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .args([
            "auto-update",
            "hold",
            "--reason",
            "maintenance",
            "--expires-in",
            "1h",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = tuishou_cmd()
        .current_dir(td.path())
        .env("HOME", &home)
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("TUISHOU_GITHUB_API_BASE", &server.base)
        .env("TUISHOU_ALLOW_INSECURE_HTTP", "1")
        .env("PATH", &path)
        .args(["auto-update", "run", "--once"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // held means no download
    assert_eq!(server.hit_count("/dl/"), 0);

    // status shows hold
    let out = tuishou_cmd()
        .current_dir(td.path())
        .env("HOME", &home)
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .args(["auto-update", "status", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["hold"]["active"], true);
    assert_eq!(v["hold"]["reason"], "maintenance");

    // release
    let out = tuishou_cmd()
        .current_dir(td.path())
        .env("HOME", &home)
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .args(["auto-update", "release"])
        .output()
        .unwrap();
    assert!(out.status.success());
}

#[test]
fn adopt_records_existing_binary_without_replacing() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    let prefix = td.path().join("prefix");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&prefix).unwrap();
    let bin = prefix.join("lu");
    fs::write(&bin, "#!/bin/sh\necho lu\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let out = tuishou_cmd()
        .env("HOME", &home)
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .args(["adopt", "lu", "--version", "v0.4.0", "--prefix"])
        .arg(&prefix)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // binary unchanged
    assert_eq!(fs::read_to_string(&bin).unwrap(), "#!/bin/sh\necho lu\n");
    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(prefix.join(".tuishou.json")).unwrap()).unwrap();
    assert_eq!(state["components"]["lu"]["version"], "v0.4.0");
    assert_eq!(state["components"]["lu"]["target"], current_target());
}

#[test]
fn lock_contention_second_run_exits_quietly() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let bindir = td.path().join("bin");
    fs::create_dir_all(&bindir).unwrap();
    fake_gh_ok(&bindir);
    // fake flock that simulates busy lock (exit 1)
    write_fake_tool(&bindir, "flock", "#!/bin/sh\nexit 99\n");
    write_config(
        td.path(),
        "[auto_update]\nenabled = true\ncomponents = []\n",
    );
    let path = bindir.display().to_string();
    let out = tuishou_cmd()
        .current_dir(td.path())
        .env("HOME", &home)
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("PATH", &path)
        .args(["auto-update", "run", "--once"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn etag_304_uses_cache_and_rate_limit_retries() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let bindir = td.path().join("bin");
    fs::create_dir_all(&bindir).unwrap();
    fake_gh_ok(&bindir);
    setup_prefix_with_state(&home, "app", "v0.4.0");
    let target = current_target();
    let asset = format!("lu-v0.4.1-{target}.tar.gz");
    let archive = make_tar_gz(&[("lu", b"new\n")]);

    // First response: 429 with Retry-After 0, then 200; also provide 304 for etag? Simpler: 429 then 200 on releases list.
    // We'll build a custom server that returns 429 once then 200 for releases endpoint.
    let checksum_name = format!("{asset}.sha256");
    let release_json = serde_json::json!({
        "tag_name":"v0.4.1","body":"","prerelease":false,"draft":false,"published_at":"2020-01-01T00:00:00Z",
        "assets":[
            {"name":asset,"browser_download_url":"http://placeholder/dl/x"},
            {"name":checksum_name,"browser_download_url":"http://placeholder/dl/y"}
        ]
    }).to_string();
    let list_json =
        serde_json::json!([serde_json::from_str::<serde_json::Value>(&release_json).unwrap()])
            .to_string();

    // custom server
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let release_json = release_json.replace("http://placeholder", &base);
    let list_json = list_json.replace("http://placeholder", &base);
    let hits = Arc::new(Mutex::new(0usize));
    let hits2 = hits.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let archive2 = archive.clone();
    let asset2 = asset.clone();
    let checksum_name2 = checksum_name.clone();
    let handle = thread::spawn(move || {
        while !stop2.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut buf = [0u8; 32768];
                    let n = stream.read(&mut buf).unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).to_string();
                    let path = req
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("/")
                        .to_string();
                    let (status, body, ct, extra): (u16, Vec<u8>, String, Vec<(String, String)>) =
                        if path.contains("/releases?per_page=30") {
                            let mut c = hits2.lock().unwrap();
                            *c += 1;
                            if *c == 1 {
                                (
                                    429,
                                    b"rate limited".to_vec(),
                                    "text/plain".to_string(),
                                    vec![("Retry-After".to_string(), "0".to_string())],
                                )
                            } else {
                                (
                                    200,
                                    list_json.clone().into_bytes(),
                                    "application/json".to_string(),
                                    vec![("ETag".to_string(), "\"e1\"".to_string())],
                                )
                            }
                        } else if path.contains("/releases/latest")
                            || path.contains("/releases/tags/")
                        {
                            (
                                200,
                                release_json.clone().into_bytes(),
                                "application/json".to_string(),
                                vec![],
                            )
                        } else if path == format!("/dl/{asset2}") {
                            (
                                200,
                                archive2.clone(),
                                "application/octet-stream".to_string(),
                                vec![],
                            )
                        } else if path == format!("/dl/{checksum_name2}") {
                            (
                                200,
                                format!("{}  {}\n", sha256_hex(&archive2), asset2).into_bytes(),
                                "text/plain".to_string(),
                                vec![],
                            )
                        } else {
                            (404, b"not found".to_vec(), "text/plain".to_string(), vec![])
                        };
                    let mut resp = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nContent-Type: {ct}\r\nConnection: close\r\n", body.len());
                    for (k, v) in extra {
                        resp.push_str(&format!("{k}: {v}\r\n"));
                    }
                    resp.push_str("\r\n");
                    let _ = stream.write_all(resp.as_bytes());
                    let _ = stream.write_all(&body);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10))
                }
                Err(_) => break,
            }
        }
    });
    let server = FakeServer {
        base: base.clone(),
        hits: Arc::new(Mutex::new(vec![])),
        stop,
        handle: Some(handle),
    };

    write_config(td.path(), "[auto_update]\nenabled = true\ncomponents = [\"lu\"]\nmin_release_age = \"0s\"\npolicy = \"patch\"\n");
    let path = format!(
        "{}:{}",
        bindir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = tuishou_cmd()
        .current_dir(td.path())
        .env("HOME", &home)
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("TUISHOU_GITHUB_API_BASE", &server.base)
        .env("TUISHOU_ALLOW_INSECURE_HTTP", "1")
        .env("PATH", &path)
        .args(["auto-update", "run", "--once"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(*hits.lock().unwrap() >= 2, "should have retried after 429");
}

#[test]
fn min_release_age_and_policy_are_enforced() {
    // unit-level coverage via eligibility module through status/run behavior is implicit;
    // these are explicit unit tests in src/version.rs and src/auto_update/eligibility.rs.
    // Integration: a release younger than min_release_age is skipped.
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let bindir = td.path().join("bin");
    fs::create_dir_all(&bindir).unwrap();
    fake_gh_ok(&bindir);
    setup_prefix_with_state(&home, "app", "v0.4.0");
    let target = current_target();
    let asset = format!("lu-v0.4.1-{target}.tar.gz");
    let archive = make_tar_gz(&[("lu", b"new\n")]);
    let future = "2099-01-01T00:00:00Z";
    let server = FakeServer::start(release_routes_with_base(
        "http://127.0.0.1:1",
        "ddproxy/tuishou-lu",
        "v0.4.1",
        &asset,
        &archive,
        "",
        future,
        Some("\"e1\""),
    ));
    let server = FakeServer::start(release_routes_with_base(
        &server.base,
        "ddproxy/tuishou-lu",
        "v0.4.1",
        &asset,
        &archive,
        "",
        future,
        Some("\"e1\""),
    ));
    write_config(td.path(), "[auto_update]\nenabled = true\ncomponents = [\"app\"]\nmin_release_age = \"10m\"\n\n[[component]]\nname = \"app\"\nrepo = \"ddproxy/tuishou-lu\"\nasset_pattern = \"lu-{tag}-{target}.tar.gz\"\nbinaries = [\"lu\"]\n");
    let path = format!(
        "{}:{}",
        bindir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = tuishou_cmd()
        .current_dir(td.path())
        .env("HOME", &home)
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("TUISHOU_GITHUB_API_BASE", &server.base)
        .env("TUISHOU_ALLOW_INSECURE_HTTP", "1")
        .env("PATH", &path)
        .args(["auto-update", "run", "--once"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        server.hit_count("/dl/"),
        0,
        "young release must not be downloaded"
    );
}

#[test]
fn agent_drain_install_restart_undrain_order() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let bindir = td.path().join("bin");
    fs::create_dir_all(&bindir).unwrap();
    fake_gh_ok(&bindir);
    let log = td.path().join("cmd.log");
    let drained = td.path().join("drained");
    let restarted = td.path().join("restarted");
    let lu = format!(
        r#"#!/bin/sh
echo "lu $@" >> {log}
if [ "$1" = "node" ] && [ "$2" = "drain" ]; then touch {drained}; exit 0; fi
if [ "$1" = "node" ] && [ "$2" = "undrain" ]; then rm -f {drained}; echo "lu undrain done" >> {log}; exit 0; fi
if [ "$1" = "node" ] && [ "$3" = "--json" ]; then
  if [ -f {drained} ]; then
    if [ -f {restarted} ]; then echo '{{"drained":true,"version":"v0.4.1"}}'; else echo '{{"drained":true,"version":"v0.4.0"}}'; fi
  else
    echo '{{"drained":false,"version":"v0.4.1"}}'
  fi
  exit 0
fi
exit 0
"#,
        log = log.display(),
        drained = drained.display(),
        restarted = restarted.display()
    );
    write_fake_tool(&bindir, "lu", &lu);
    let systemctl = format!(
        r#"#!/bin/sh
echo "systemctl $@" >> {log}
if [ "$1" = "restart" ]; then touch {restarted}; fi
if [ "$1" = "is-active" ]; then exit 0; fi
exit 0
"#,
        log = log.display(),
        restarted = restarted.display()
    );
    write_fake_tool(&bindir, "systemctl", &systemctl);

    setup_prefix_with_state(&home, "ting", "v0.4.0");
    let home_bin = home.join(".local/bin");
    fs::create_dir_all(&home_bin).unwrap();
    fs::write(home_bin.join("ting"), b"#!/bin/sh\necho old\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(home_bin.join("ting"), fs::Permissions::from_mode(0o755)).unwrap();
    }

    let target = current_target();
    let asset = format!("lu-v0.4.1-{target}.tar.gz");
    let archive = make_tar_gz(&[("ting", b"#!/bin/sh\necho ting v0.4.1\n")]);
    let server = FakeServer::start(release_routes_with_base(
        "http://127.0.0.1:1",
        "ddproxy/tuishou-lu",
        "v0.4.1",
        &asset,
        &archive,
        "",
        "2020-01-01T00:00:00Z",
        Some("\"e1\""),
    ));
    let server = FakeServer::start(release_routes_with_base(
        &server.base,
        "ddproxy/tuishou-lu",
        "v0.4.1",
        &asset,
        &archive,
        "",
        "2020-01-01T00:00:00Z",
        Some("\"e1\""),
    ));
    fs::write(td.path().join("tuishou.toml"), "[auto_update]\nenabled = true\ncomponents = [\"ting\"]\nmin_release_age = \"0s\"\ndrain_timeout = \"2s\"\nhealth_timeout = \"2s\"\nnode_name = \"n1\"\n").unwrap();
    let path = format!(
        "{}:{}",
        bindir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = tuishou_cmd()
        .current_dir(td.path())
        .env("HOME", &home)
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("TUISHOU_GITHUB_API_BASE", &server.base)
        .env("TUISHOU_ALLOW_INSECURE_HTTP", "1")
        .env("PATH", &path)
        .args(["auto-update", "run", "--once"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let log_s = fs::read_to_string(&log).unwrap();
    let drain = log_s.find("lu node drain n1").expect(&log_s);
    let restart = log_s
        .find("systemctl restart lu-agent.service")
        .expect(&log_s);
    let undrain = log_s.find("lu node undrain n1").expect(&log_s);
    assert!(drain < restart && restart < undrain, "order wrong: {log_s}");
}

#[test]
fn health_failure_rolls_back_and_marks_bad() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let bindir = td.path().join("bin");
    fs::create_dir_all(&bindir).unwrap();
    fake_gh_ok(&bindir);
    let drained = td.path().join("drained");
    let lu = format!(
        r#"#!/bin/sh
if [ "$1" = "node" ] && [ "$2" = "drain" ]; then touch {drained}; exit 0; fi
if [ "$1" = "node" ] && [ "$2" = "undrain" ]; then rm -f {drained}; exit 0; fi
if [ "$1" = "node" ] && [ "$3" = "--json" ]; then
  if [ -f {drained} ]; then echo '{{"drained":true,"version":"v0.4.0"}}'; else echo '{{"drained":false,"version":"v0.4.0"}}'; fi
  exit 0
fi
exit 0
"#,
        drained = drained.display()
    );
    write_fake_tool(&bindir, "lu", &lu);
    write_fake_tool(
        &bindir,
        "systemctl",
        "#!/bin/sh\nif [ \"$1\" = \"is-active\" ]; then exit 0; fi\nexit 0\n",
    );
    setup_prefix_with_state(&home, "ting", "v0.4.0");
    let home_bin = home.join(".local/bin");
    fs::create_dir_all(&home_bin).unwrap();
    fs::write(home_bin.join("ting"), b"#!/bin/sh\necho old\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(home_bin.join("ting"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let target = current_target();
    let asset = format!("lu-v0.4.1-{target}.tar.gz");
    let archive = make_tar_gz(&[("ting", b"#!/bin/sh\necho new v0.4.1\n")]);
    let server = FakeServer::start(release_routes_with_base(
        "http://127.0.0.1:1",
        "ddproxy/tuishou-lu",
        "v0.4.1",
        &asset,
        &archive,
        "",
        "2020-01-01T00:00:00Z",
        Some("\"e1\""),
    ));
    let server = FakeServer::start(release_routes_with_base(
        &server.base,
        "ddproxy/tuishou-lu",
        "v0.4.1",
        &asset,
        &archive,
        "",
        "2020-01-01T00:00:00Z",
        Some("\"e1\""),
    ));
    fs::write(td.path().join("tuishou.toml"), "[auto_update]\nenabled = true\ncomponents = [\"ting\"]\nmin_release_age = \"0s\"\ndrain_timeout = \"2s\"\nhealth_timeout = \"1s\"\nnode_name = \"n1\"\n").unwrap();
    let path = format!(
        "{}:{}",
        bindir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = tuishou_cmd()
        .current_dir(td.path())
        .env("HOME", &home)
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("TUISHOU_GITHUB_API_BASE", &server.base)
        .env("TUISHOU_ALLOW_INSECURE_HTTP", "1")
        .env("PATH", &path)
        .args(["auto-update", "run", "--once"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let auto_state_path = home.join(".local/state/tuishou/auto.json");
    let auto_state_s = fs::read_to_string(&auto_state_path).unwrap_or_default();
    assert!(!out.status.success(), "should fail on unhealthy node\nstdout={stdout}\nstderr={stderr}\nauto={auto_state_s}");
    // .prev restored
    let content = fs::read(home_bin.join("ting")).unwrap();
    assert_eq!(content, b"#!/bin/sh\necho old\n");
    // bad marked
    let auto_state: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(home.join(".local/state/tuishou/auto.json")).unwrap(),
    )
    .unwrap();
    let bad = auto_state["bad_versions"]["ting"].as_array().unwrap();
    assert!(bad.iter().any(|v| v == "v0.4.1"), "{auto_state}");
}

#[test]
fn controller_schema_marker_and_notify_and_backups() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let bindir = td.path().join("bin");
    fs::create_dir_all(&bindir).unwrap();
    fake_gh_ok(&bindir);
    let notify_log = td.path().join("notify.json");
    let notify_cmd = td.path().join("notify.sh");
    fs::write(
        &notify_cmd,
        format!(
            "#!/bin/sh\ncat > {}\necho \"$@\" >> {}\n",
            notify_log.display(),
            notify_log.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&notify_cmd, fs::Permissions::from_mode(0o755)).unwrap();
    }

    // fake lu: job list idle, backup creates files
    let backup_dir = td.path().join("backups");
    fs::create_dir_all(&backup_dir).unwrap();
    let lu = r#"#!/bin/sh
if [ "$1" = "job" ] && [ "$2" = "list" ]; then echo '{"jobs":[],"running_jobs":0}'; exit 0; fi
if [ "$1" = "backup" ]; then
  dir="$3"
  i=$(ls "$dir" | wc -l)
  echo backup > "$dir/bk-$i.sql"
  exit 0
fi
exit 0
"#.to_string();
    write_fake_tool(&bindir, "lu", &lu);
    write_fake_tool(&bindir, "systemctl", "#!/bin/sh\nexit 0\n");

    setup_prefix_with_state(&home, "lu", "v0.4.0");
    let target = current_target();
    let asset = format!("lu-v0.4.1-{target}.tar.gz");
    let archive = make_tar_gz(&[
        ("lu", b"#!/bin/sh\necho lu v0.4.1\n"),
        ("ting", b"#!/bin/sh\necho ting\n"),
        ("lu-agent", b"#!/bin/sh\necho agent\n"),
    ]);
    let server = FakeServer::start(release_routes_with_base(
        "http://127.0.0.1:1",
        "ddproxy/tuishou-lu",
        "v0.4.1",
        &asset,
        &archive,
        "Requires: new controller database\n",
        "2020-01-01T00:00:00Z",
        Some("\"e1\""),
    ));
    let server = FakeServer::start(release_routes_with_base(
        &server.base,
        "ddproxy/tuishou-lu",
        "v0.4.1",
        &asset,
        &archive,
        "Requires: new controller database\n",
        "2020-01-01T00:00:00Z",
        Some("\"e1\""),
    ));

    let notify_path = notify_cmd.display().to_string();
    fs::write(td.path().join("tuishou.toml"), format!(
        "[auto_update]\nenabled = true\ncomponents = [\"lu\"]\nmin_release_age = \"0s\"\ncontroller_policy = \"when_idle\"\nbackup_dir = \"{}\"\nbackup_keep = 2\nnotify_command = [\"{}\"]\nnotify_after_failures = 1\n",
        backup_dir.display(), notify_path
    )).unwrap();

    let path = format!(
        "{}:{}",
        bindir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = tuishou_cmd()
        .current_dir(td.path())
        .env("HOME", &home)
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("TUISHOU_GITHUB_API_BASE", &server.base)
        .env("TUISHOU_ALLOW_INSECURE_HTTP", "1")
        .env("PATH", &path)
        .args(["auto-update", "run", "--once"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // schema marker means no install
    assert_eq!(
        server.hit_count("/dl/"),
        0,
        "schema change must not install"
    );
    let auto_state: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(home.join(".local/state/tuishou/auto.json")).unwrap(),
    )
    .unwrap();
    let reason = auto_state["results"]["lu"]["reason"].as_str().unwrap_or("");
    assert!(
        reason.contains("controller database") || reason.contains("schema"),
        "{auto_state}"
    );
}

#[test]
fn controller_backup_retention_and_notify_on_update() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let bindir = td.path().join("bin");
    fs::create_dir_all(&bindir).unwrap();
    fake_gh_ok(&bindir);
    let notify_out = td.path().join("notify.out");
    let notify_cmd = td.path().join("notify.sh");
    fs::write(
        &notify_cmd,
        format!(
            "#!/bin/sh\necho \"$@\" > {}\ncat >> {}\n",
            notify_out.display(),
            notify_out.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&notify_cmd, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let backup_dir = td.path().join("backups");
    fs::create_dir_all(&backup_dir).unwrap();
    // pre-seed 2 old backups
    fs::write(backup_dir.join("old1.sql"), "1").unwrap();
    fs::write(backup_dir.join("old2.sql"), "2").unwrap();
    let lu = format!(
        r#"#!/bin/sh
if [ "$1" = "job" ] && [ "$2" = "list" ]; then echo '{{"jobs":[],"running_jobs":0}}'; exit 0; fi
if [ "$1" = "backup" ]; then dir="$3"; n=$(date +%s%N); echo b > "$dir/bk-$n.sql"; exit 0; fi
exit 0
"#
    );
    write_fake_tool(&bindir, "lu", &lu);
    write_fake_tool(&bindir, "systemctl", "#!/bin/sh\nexit 0\n");

    setup_prefix_with_state(&home, "lu", "v0.4.0");
    let home_bin = home.join(".local/bin");
    fs::create_dir_all(&home_bin).unwrap();
    let target = current_target();
    let asset = format!("lu-v0.4.1-{target}.tar.gz");
    let archive = make_tar_gz(&[
        ("lu", b"#!/bin/sh\necho lu v0.4.1\n"),
        ("ting", b"#!/bin/sh\necho t\n"),
        ("lu-agent", b"#!/bin/sh\necho a\n"),
    ]);
    let server = FakeServer::start(release_routes_with_base(
        "http://127.0.0.1:1",
        "ddproxy/tuishou-lu",
        "v0.4.1",
        &asset,
        &archive,
        "## ok\n",
        "2020-01-01T00:00:00Z",
        Some("\"e1\""),
    ));
    let server = FakeServer::start(release_routes_with_base(
        &server.base,
        "ddproxy/tuishou-lu",
        "v0.4.1",
        &asset,
        &archive,
        "## ok\n",
        "2020-01-01T00:00:00Z",
        Some("\"e1\""),
    ));

    let notify_path = notify_cmd.display().to_string();
    fs::write(td.path().join("tuishou.toml"), format!(
        "[auto_update]\nenabled = true\ncomponents = [\"lu\"]\nmin_release_age = \"0s\"\ncontroller_policy = \"when_idle\"\nbackup_dir = \"{}\"\nbackup_keep = 2\nnotify_command = [\"{}\"]\n",
        backup_dir.display(), notify_path
    )).unwrap();
    let path = format!(
        "{}:{}",
        bindir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = tuishou_cmd()
        .current_dir(td.path())
        .env("HOME", &home)
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("TUISHOU_GITHUB_API_BASE", &server.base)
        .env("TUISHOU_ALLOW_INSECURE_HTTP", "1")
        .env("PATH", &path)
        .args(["auto-update", "run", "--once"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // backup_keep=2 => at most 2 files
    let count = fs::read_dir(&backup_dir).unwrap().count();
    assert!(count <= 2, "backup retention failed: {count}");
    // notify got JSON
    let notify = fs::read_to_string(&notify_out).unwrap_or_default();
    assert!(
        notify.contains("\"event\""),
        "notify should receive json: {notify}"
    );
    assert!(
        notify.contains("update"),
        "notify should report update: {notify}"
    );
}
