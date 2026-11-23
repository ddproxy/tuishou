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
}

struct FakeServer {
    base: String,
    routes: Arc<Mutex<Vec<Route>>>,
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
        let routes = Arc::new(Mutex::new(routes));
        let routes2 = routes.clone();
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
                        let route = routes2
                            .lock()
                            .unwrap()
                            .iter()
                            .find(|r| r.path == path)
                            .cloned();
                        let (status, body, ct) = match route {
                            Some(r) => (r.status, r.body, r.content_type),
                            None => (404, b"not found".to_vec(), "text/plain".to_string()),
                        };
                        let resp = format!(
                            "HTTP/1.1 {status} {}\r\nContent-Length: {}\r\nContent-Type: {ct}\r\nConnection: close\r\n\r\n",
                            status_text(status),
                            body.len()
                        );
                        let _ = stream.write_all(resp.as_bytes());
                        let _ = stream.write_all(&body);
                    }
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            || e.kind() == std::io::ErrorKind::Interrupted =>
                    {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => thread::sleep(Duration::from_millis(10)),
                }
            }
        });
        Self {
            base,
            routes,
            hits,
            stop,
            handle: Some(handle),
        }
    }

    fn set_routes(&self, routes: Vec<Route>) {
        *self.routes.lock().unwrap() = routes;
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
        404 => "Not Found",
        500 => "Internal Server Error",
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
        other => panic!("unsupported test target: {other:?}"),
    }
}

fn tuishou_cmd() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tuishou"));
    cmd.env("TUISHOU_FAKE_ATTESTATION", "1");
    cmd
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
    base: &str,
    repo: &str,
    tag: &str,
    asset: &str,
    archive: &[u8],
    body: &str,
) -> Vec<Route> {
    let checksum_name = format!("{asset}.sha256");
    let release = serde_json::json!({
        "target_commitish":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","tag_name": tag,
        "body": body,
        "prerelease": tag.contains('-'),
        "draft": false,
        "published_at": "2020-01-01T00:00:00Z",
        "assets": [
            {"name": asset, "browser_download_url": format!("{base}/dl/{asset}")},
            {"name": checksum_name, "browser_download_url": format!("{base}/dl/{checksum_name}")}
        ]
    })
    .to_string();
    let list = format!("[{release}]");
    vec![
        Route {
            path: format!("/repos/{repo}/releases/latest"),
            status: 200,
            body: release.clone().into_bytes(),
            content_type: "application/json".into(),
        },
        Route {
            path: format!("/repos/{repo}/releases/tags/{tag}"),
            status: 200,
            body: release.into_bytes(),
            content_type: "application/json".into(),
        },
        Route {
            path: format!("/repos/{repo}/releases?per_page=30"),
            status: 200,
            body: list.into_bytes(),
            content_type: "application/json".into(),
        },
        Route {
            path: format!("/dl/{asset}"),
            status: 200,
            body: archive.to_vec(),
            content_type: "application/octet-stream".into(),
        },
        Route {
            path: format!("/dl/{checksum_name}"),
            status: 200,
            body: format!("{}  {}\n", sha256_hex(archive), asset).into_bytes(),
            content_type: "text/plain".into(),
        },
    ]
}

fn write_config(
    dir: &Path,
    health_url: &str,
    backup_dir: &Path,
    opt_in: &[&str],
    extra: &str,
) -> PathBuf {
    let opt_in_blocks = opt_in
        .iter()
        .map(|name| format!("[[component]]\nname = \"{name}\"\nauto_update = true\n"))
        .collect::<String>();
    let contents = format!(
        "[auto_update]\nenabled = true\ncomponents = [\"reiki\", \"lu\"]\nmin_release_age = \"0s\"\ndrain_timeout = \"2s\"\ncontroller_wait_timeout = \"2s\"\nhealth_timeout = \"1s\"\nheartbeat_max_age = \"5m\"\nnode_name = \"n1\"\nreiki_health_url = \"{health_url}\"\ncontroller_policy = \"when_idle\"\nbackup_dir = \"{backup}\"\n{extra}\n{opt_in_blocks}",
        backup = backup_dir.display()
    );
    let path = dir.join("tuishou.toml");
    fs::write(&path, contents).unwrap();
    path
}

fn setup_prefix(home: &Path, component: &str, version: &str) -> PathBuf {
    let state_dir = home.join(".local/state/tuishou");
    fs::create_dir_all(&state_dir).unwrap();
    let state = serde_json::json!({
        "components": {
            component: {
                "version": version,
                "target": current_target(),
                "checksum": "0".repeat(64),
                "installed_at": "2020-01-01T00:00:00Z"
            }
        }
    });
    fs::write(state_dir.join("installed.json"), state.to_string()).unwrap();
    let bin = home.join(".local/bin");
    fs::create_dir_all(&bin).unwrap();
    for name in ["lu", "ting", "reiki"] {
        fs::write(bin.join(name), format!("#!/bin/sh\necho old {name}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(bin.join(name), fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    bin
}

struct Shims {
    bin: PathBuf,
    log: PathBuf,
    drained: PathBuf,
    restarted: PathBuf,
    role: PathBuf,
    busy: PathBuf,
    busy_once: PathBuf,
    backup_fail: PathBuf,
    unreachable: PathBuf,
    stale: PathBuf,
}

impl Shims {
    fn new(dir: &Path) -> Self {
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let shims = Self {
            log: dir.join("cmd.log"),
            drained: dir.join("drained"),
            restarted: dir.join("restarted"),
            role: dir.join("role"),
            busy: dir.join("busy"),
            busy_once: dir.join("busy_once"),
            backup_fail: dir.join("backup_fail"),
            unreachable: dir.join("unreachable"),
            stale: dir.join("stale"),
            bin: bin.clone(),
        };
        fs::write(&shims.role, "agent\n").unwrap();

        let lu = format!(
            r#"#!/bin/sh
echo "lu $@" >> {log}
if [ "$1" = "drain" ]; then touch {drained}; exit 0; fi
if [ "$1" = "undrain" ]; then rm -f {drained}; exit 0; fi
if [ "$1" = "nodes" ] && [ "$2" = "--json" ]; then
  SEEN=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  if [ ! -f {drained} ]; then
    VER=0.4.0; DRAIN=false; JOB='{{"id":"j1"}}'
  elif [ -f {restarted} ] && [ ! -f {stale} ]; then
    VER=0.4.1; DRAIN=true; JOB=null
  else
    VER=0.4.0; DRAIN=true; JOB=null
  fi
  printf '[{{"name":"n1","version":"%s","draining":%s,"current_job":%s,"online":true,"last_seen":"%s"}}]\n' "$VER" "$DRAIN" "$JOB" "$SEEN"
  exit 0
fi
if [ "$1" = "status" ] && [ "$2" = "--json" ]; then
  if [ -f {unreachable} ]; then exit 1; fi
  VER=0.4.0
  if [ -f {restarted} ]; then VER=0.4.1; fi
  RUNNING=0
  if [ -f {busy} ]; then
    RUNNING=1
    if [ -f {busy_once} ]; then rm -f {busy} {busy_once}; fi
  fi
  printf '{{"version":"%s","jobs":{{"leased":0,"running":%s,"recovering":0,"needs_input":0}},"nodes":1,"nodes_online":1}}\n' "$VER" "$RUNNING"
  exit 0
fi
if [ "$1" = "backup" ]; then
  if [ -f {backup_fail} ]; then exit 1; fi
  while [ $# -gt 0 ]; do
    if [ "$1" = "--out" ]; then printf 'backup payload' > "$2"; exit 0; fi
    shift
  done
fi
exit 0
"#,
            log = shims.log.display(),
            drained = shims.drained.display(),
            restarted = shims.restarted.display(),
            stale = shims.stale.display(),
            unreachable = shims.unreachable.display(),
            busy = shims.busy.display(),
            busy_once = shims.busy_once.display(),
            backup_fail = shims.backup_fail.display(),
        );
        write_fake_tool(&bin, "lu", &lu);

        let systemctl = format!(
            r#"#!/bin/sh
echo "systemctl $@" >> {log}
ROLE=$(cat {role} 2>/dev/null || echo agent)
UNIT=""
if [ "$1" = "cat" ]; then UNIT=$2; fi
if [ "$1" = "is-enabled" ] && [ "$2" = "--quiet" ]; then UNIT=$3; fi
if [ -n "$UNIT" ]; then
  if [ "$ROLE" = "agent" ] && [ "$UNIT" = "ting.service" ]; then exit 0; fi
  if [ "$ROLE" = "controller" ] && [ "$UNIT" = "lu-controller.service" ]; then exit 0; fi
  exit 1
fi
if [ "$1" = "restart" ] && [ "$2" = "ting.service" ]; then touch {restarted}; fi
if [ "$1" = "restart" ] && [ "$2" = "lu-controller.service" ]; then touch {restarted}; fi
if [ "$1" = "is-active" ] && [ "$2" = "--quiet" ]; then exit 0; fi
exit 0
"#,
            log = shims.log.display(),
            role = shims.role.display(),
            restarted = shims.restarted.display(),
        );
        write_fake_tool(&bin, "systemctl", &systemctl);

        let reiki = format!(
            r#"#!/bin/sh
echo "reiki $@" >> {log}
exit 0
"#,
            log = shims.log.display(),
        );
        write_fake_tool(&bin, "reiki", &reiki);
        shims
    }

    fn path(&self) -> String {
        format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        )
    }

    fn log(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn set_role(&self, role: &str) {
        fs::write(&self.role, format!("{role}\n")).unwrap();
    }
}

struct Fixture {
    td: PathBuf,
    home: PathBuf,
    bin: PathBuf,
    server: FakeServer,
    shims: Shims,
}

impl Fixture {
    fn new(repo: &str, tag: &str, asset: &str, archive: &[u8], opt_in: &[&str]) -> Self {
        Self::new_with_health(repo, tag, asset, archive, opt_in, 200)
    }

    fn new_with_health(
        repo: &str,
        tag: &str,
        asset: &str,
        archive: &[u8],
        opt_in: &[&str],
        health_status: u16,
    ) -> Self {
        let td = tempfile::tempdir().unwrap().keep();
        let home = td.join("home");
        fs::create_dir_all(&home).unwrap();
        let bin = setup_prefix(
            &home,
            if repo.ends_with("reiki") {
                "reiki"
            } else {
                "lu"
            },
            "v0.4.0",
        );
        let server = FakeServer::start(Vec::new());
        let base = server.base.clone();
        let mut routes = release_routes(&base, repo, tag, asset, archive, "## ok\n");
        routes.push(Route {
            path: "/health".to_string(),
            status: health_status,
            body: if health_status == 200 {
                b"ok".to_vec()
            } else {
                b"unhealthy".to_vec()
            },
            content_type: "text/plain".into(),
        });
        server.set_routes(routes);
        let backup_dir = td.join("backups");
        write_config(&td, &server.base, &backup_dir, opt_in, "");
        let shims = Shims::new(&td);
        Self {
            td,
            home,
            bin,
            server,
            shims,
        }
    }

    fn run_cycle(&self) -> std::process::Output {
        tuishou_cmd()
            .current_dir(&self.td)
            .env("HOME", &self.home)
            .env("XDG_STATE_HOME", self.home.join(".local/state"))
            .env("TUISHOU_GITHUB_API_BASE", &self.server.base)
            .env("TUISHOU_ALLOW_INSECURE_HTTP", "1")
            .env("TUISHOU_TEST_REIKI_LOG", self.td.join("reiki-staged.log"))
            .env("PATH", self.shims.path())
            .args(["auto-update", "run", "--once"])
            .output()
            .unwrap()
    }

    fn auto_state(&self) -> serde_json::Value {
        let path = self.home.join(".local/state/tuishou/auto.json");
        serde_json::from_str(&fs::read_to_string(path).unwrap_or_default()).unwrap_or_default()
    }
}

fn lu_archive(version: &str) -> Vec<u8> {
    make_tar_gz(&[
        ("lu", format!("#!/bin/sh\necho {version}\n").as_bytes()),
        (
            "ting",
            format!("#!/bin/sh\necho {version} agent\n").as_bytes(),
        ),
    ])
}

fn reiki_archive(fail_check: bool) -> Vec<u8> {
    let check = format!(
        "#!/bin/sh\nif [ -n \"$TUISHOU_TEST_REIKI_LOG\" ]; then echo \"staged reiki $@\" >> \"$TUISHOU_TEST_REIKI_LOG\"; fi\nif [ \"$1\" = \"--version\" ]; then echo reiki 0.4.1; exit 0; fi\nif [ \"$1\" = \"check\" ]; then {}; fi\nexit 0\n",
        if fail_check { "exit 1" } else { "exit 0" }
    );
    make_tar_gz(&[("reiki", check.as_bytes())])
}

fn assert_no_legacy_lu_commands(log: &str) {
    for legacy in [
        "lu node ",
        "lu node drain",
        "lu node undrain",
        "lu job list",
    ] {
        assert!(
            !log.contains(legacy),
            "legacy command {legacy} in log: {log}"
        );
    }
}

#[test]
fn components_list_alone_does_not_enable_auto_update() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let archive = lu_archive("v0.4.1");
    let fx = Fixture::new(
        "ddproxy/tuishou-lu",
        "v0.4.1",
        &format!("lu-v0.4.1-{}.tar.gz", current_target()),
        &archive,
        &[],
    );
    let out = fx.run_cycle();
    assert!(out.status.success());
    assert_eq!(
        fx.server.hit_count("/dl/"),
        0,
        "component without auto_update must not be downloaded"
    );
}

#[test]
fn stable_channel_refuses_prerelease() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let tag = "v0.4.1-rc.1";
    let asset = format!("lu-{tag}-{}.tar.gz", current_target());
    let archive = lu_archive(tag);
    let fx = Fixture::new("ddproxy/tuishou-lu", tag, &asset, &archive, &["lu"]);
    let out = fx.run_cycle();
    assert!(out.status.success());
    assert_eq!(fx.server.hit_count("/dl/"), 0);
    let state = fx.auto_state();
    assert!(
        format!("{state}").contains("prerelease"),
        "unexpected state: {state}"
    );
}

#[test]
fn agent_drains_checks_health_and_undrains_in_order() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let asset = format!("lu-v0.4.1-{}.tar.gz", current_target());
    let fx = Fixture::new(
        "ddproxy/tuishou-lu",
        "v0.4.1",
        &asset,
        &lu_archive("v0.4.1"),
        &["lu"],
    );
    let out = fx.run_cycle();
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let log = fx.shims.log();
    assert_no_legacy_lu_commands(&log);
    let drain = log.find("lu drain n1").expect(&log);
    let poll = log.find("lu nodes --json").expect(&log);
    let restart = log.find("systemctl restart ting.service").expect(&log);
    let active = log
        .find("systemctl is-active --quiet ting.service")
        .expect(&log);
    let undrain = log.find("lu undrain n1").expect(&log);
    assert!(
        drain < poll && poll < restart && restart < active && active < undrain,
        "{log}"
    );
    assert_eq!(
        fs::read(fx.bin.join("ting")).unwrap(),
        b"#!/bin/sh\necho v0.4.1 agent\n"
    );
}

#[test]
fn unhealthy_agent_rolls_back_marks_bad_and_undrains() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let asset = format!("lu-v0.4.1-{}.tar.gz", current_target());
    let fx = Fixture::new(
        "ddproxy/tuishou-lu",
        "v0.4.1",
        &asset,
        &lu_archive("v0.4.1"),
        &["lu"],
    );
    // The node never reports the new version, so the first health check fails;
    // it keeps reporting the old version so restored health verifies.
    fs::write(&fx.shims.stale, "").unwrap();
    let out = fx.run_cycle();
    assert!(
        !out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let state = fx.auto_state();
    assert!(state["bad_versions"]["lu"][0] == "v0.4.1", "{state}");
    let log = fx.shims.log();
    assert_eq!(
        log.matches("systemctl restart ting.service").count(),
        2,
        "{log}"
    );
    assert!(log.contains("lu undrain n1"), "{log}");
    assert_eq!(
        fs::read(fx.bin.join("lu")).unwrap(),
        b"#!/bin/sh\necho old lu\n"
    );
}

#[test]
fn controller_waits_for_idle_backs_up_and_uses_detected_unit() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let asset = format!("lu-v0.4.1-{}.tar.gz", current_target());
    let fx = Fixture::new(
        "ddproxy/tuishou-lu",
        "v0.4.1",
        &asset,
        &lu_archive("v0.4.1"),
        &["lu"],
    );
    fx.shims.set_role("controller");
    fs::write(&fx.shims.busy, "").unwrap();
    fs::write(&fx.shims.busy_once, "").unwrap();
    let out = fx.run_cycle();
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let log = fx.shims.log();
    assert!(log.contains("lu status --json"), "{log}");
    assert!(log.contains("lu backup"), "{log}");
    assert!(
        log.contains("systemctl restart lu-controller.service"),
        "{log}"
    );
    assert!(!log.contains("lu drain"), "{log}");
    assert!(fs::read_dir(fx.td.join("backups")).unwrap().any(|e| e
        .unwrap()
        .metadata()
        .unwrap()
        .len()
        > 0));
}

#[test]
fn failed_backup_refuses_controller_update() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let asset = format!("lu-v0.4.1-{}.tar.gz", current_target());
    let fx = Fixture::new(
        "ddproxy/tuishou-lu",
        "v0.4.1",
        &asset,
        &lu_archive("v0.4.1"),
        &["lu"],
    );
    fx.shims.set_role("controller");
    fs::write(&fx.shims.backup_fail, "").unwrap();
    let out = fx.run_cycle();
    assert!(!out.status.success());
    let log = fx.shims.log();
    assert!(!log.contains("systemctl restart"), "{log}");
    assert_eq!(
        fs::read(fx.bin.join("lu")).unwrap(),
        b"#!/bin/sh\necho old lu\n"
    );
}

#[test]
fn reiki_staged_config_failure_keeps_old_binary_and_marks_bad() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let asset = format!("reiki-v0.4.1-{}.tar.gz", current_target());
    let fx = Fixture::new(
        "ddproxy/reiki",
        "v0.4.1",
        &asset,
        &reiki_archive(true),
        &["reiki"],
    );
    let out = fx.run_cycle();
    assert!(
        !out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // The staged check runs with a minimal environment; its invocation is
    // reported in the failure reason rather than through a helper log.
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("reiki check --config /var/lib/reiki/reiki.toml"),
        "output: {combined}"
    );
    let log = fx.shims.log();
    assert!(!log.contains("systemctl restart reiki.service"), "{log}");
    assert!(log.contains("lu undrain n1"), "{log}");
    assert_eq!(
        fs::read(fx.bin.join("reiki")).unwrap(),
        b"#!/bin/sh\necho old reiki\n"
    );
    assert_eq!(fx.auto_state()["bad_versions"]["reiki"][0], "v0.4.1");
}

#[test]
fn failed_reiki_health_endpoint_rolls_back_and_marks_bad() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let asset = format!("reiki-v0.4.1-{}.tar.gz", current_target());
    let fx = Fixture::new_with_health(
        "ddproxy/reiki",
        "v0.4.1",
        &asset,
        &reiki_archive(false),
        &["reiki"],
        500,
    );
    let out = fx.run_cycle();
    assert!(
        !out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let log = fx.shims.log();
    assert_eq!(
        log.matches("systemctl restart reiki.service").count(),
        2,
        "log={log}\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // The node stays drained when restored health cannot be verified.
    assert!(!log.contains("lu undrain n1"), "{log}");
    assert!(fx.shims.drained.exists());
    assert_eq!(
        fs::read(fx.bin.join("reiki")).unwrap(),
        b"#!/bin/sh\necho old reiki\n"
    );
    assert_eq!(fx.auto_state()["bad_versions"]["reiki"][0], "v0.4.1");
}

#[test]
fn both_units_enabled_fails_closed() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let asset = format!("lu-v0.4.1-{}.tar.gz", current_target());
    let fx = Fixture::new(
        "ddproxy/tuishou-lu",
        "v0.4.1",
        &asset,
        &lu_archive("v0.4.1"),
        &["lu"],
    );
    let log = fx.td.join("both-units.log");
    write_fake_tool(
        &fx.shims.bin,
        "systemctl",
        &format!(
            "#!/bin/sh\necho \"systemctl $@\" >> {}\nexit 0\n",
            log.display()
        ),
    );
    let out = fx.run_cycle();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("refusing to guess"),
        "stderr={stderr} stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(fx.server.hit_count("/dl/"), 0);
}

#[test]
fn status_exposes_update_fields_in_json() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    fs::create_dir_all(home.join(".local/state/tuishou")).unwrap();
    fs::write(
        home.join(".local/state/tuishou/auto.json"),
        serde_json::json!({
            "last_check": "2026-11-22T10:00:00Z",
            "results": {
                "lu": {
                    "result": "up to date",
                    "at": "2026-11-22T10:00:00Z",
                    "version": "v0.4.1"
                }
            },
            "bad_versions": { "lu": ["v0.4.0"] }
        })
        .to_string(),
    )
    .unwrap();
    setup_prefix(&home, "lu", "v0.4.1");
    let asset = format!("lu-v0.4.1-{}.tar.gz", current_target());
    let server = FakeServer::start(Vec::new());
    let base = server.base.clone();
    let routes = release_routes(
        &base,
        "ddproxy/tuishou-lu",
        "v0.4.1",
        &asset,
        &lu_archive("v0.4.1"),
        "## ok\n",
    );
    server.set_routes(routes);
    let out = tuishou_cmd()
        .current_dir(td.path())
        .env("HOME", &home)
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("TUISHOU_GITHUB_API_BASE", &server.base)
        .env("TUISHOU_ALLOW_INSECURE_HTTP", "1")
        .arg("--json")
        .arg("status")
        .output()
        .unwrap();
    assert!(out.status.success());
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let lu = &json[1];
    assert_eq!(lu["component"], "lu");
    assert_eq!(lu["last_result"], "up to date");
    assert_eq!(lu["last_check"], "2026-11-22T10:00:00Z");
    assert_eq!(lu["bad_versions"][0], "v0.4.0");
    assert_eq!(lu["auto_update"], false);
    assert!(
        lu["installed_version"].is_string(),
        "status json: {}",
        serde_json::to_string_pretty(&json).unwrap()
    );
    assert!(lu["latest_version"].is_string());
}
