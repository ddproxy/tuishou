#![cfg(feature = "top")]
//! End-to-end coverage for the host TUI. The host itself runs inside a pty so
//! raw mode, the alternate screen and crossterm key parsing are all real.

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use nix::sys::signal::{kill, Signal};
#[cfg(unix)]
use nix::unistd::Pid;

const ROWS: u16 = 24;
const COLS: u16 = 100;
const WAIT: Duration = Duration::from_secs(6);

struct HostPty {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    output: Arc<Mutex<Vec<u8>>>,
    writer: Box<dyn Write + Send>,
    _master: Box<dyn portable_pty::MasterPty + Send>,
}

impl HostPty {
    fn start(state_dir: &Path, args: &[String]) -> Self {
        let config = state_dir.join("tuishou.toml");
        fs::write(&config, "").unwrap();
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: ROWS,
                cols: COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_tuishou"));
        command.arg("--config");
        command.arg(&config);
        command.args(["top"]);
        command.args(args);
        command.env("XDG_STATE_HOME", state_dir);
        command.env("HOME", state_dir);
        command.env("TERM", "xterm-256color");
        // An unrelated credential must never leak into host output or state.
        command.env("GITHUB_TOKEN", "top-secret-token-do-not-log");
        let child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().unwrap();
        let writer = pair.master.take_writer().unwrap();
        let output = Arc::new(Mutex::new(Vec::new()));
        let output2 = Arc::clone(&output);
        thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => output2.lock().unwrap().extend_from_slice(&buffer[..n]),
                }
            }
        });
        Self {
            child,
            output,
            writer,
            _master: pair.master,
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).unwrap();
        self.writer.flush().unwrap();
    }

    fn raw(&self) -> Vec<u8> {
        self.output.lock().unwrap().clone()
    }

    fn text(&self) -> String {
        let bytes = self.output.lock().unwrap().clone();
        let mut parser = vt100::Parser::new(ROWS, COLS, 0);
        parser.process(&bytes);
        parser.screen().contents()
    }

    fn wait_for<F: Fn(&str) -> bool>(&self, label: &str, predicate: F) -> String {
        let deadline = Instant::now() + WAIT;
        loop {
            let text = self.text();
            if predicate(&text) {
                return text;
            }
            if Instant::now() >= deadline {
                panic!("timed out waiting for {label}; screen:\n{text}");
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn wait_exit(&mut self) {
        let deadline = Instant::now() + WAIT;
        loop {
            if self.child.try_wait().unwrap().is_some() {
                return;
            }
            if Instant::now() >= deadline {
                panic!("host did not exit");
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn pid(&self) -> u32 {
        self.child.process_id().expect("host pid")
    }
}

fn write_script(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn counter_script() -> &'static str {
    r#"#!/usr/bin/env bash
stty raw -echo 2>/dev/null || true
name="$1"
pidfile="$2"
printf '%s\n' "$$" > "$pidfile"
count=0
trap 'exit 0' HUP TERM
draw() { printf '\033[H\033[2JFAKE %s count=%d\r\n' "$name" "$count"; }
draw
while IFS= read -r -N 1 key; do
  case "$key" in
    +) count=$((count + 1)); draw ;;
    x) exit 7 ;;
  esac
done
"#
}

fn quoted(path: &Path) -> String {
    format!("'{}'", path.display())
}

fn state_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tuishou-top-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn top_state_path(dir: &Path) -> PathBuf {
    let state_dir = dir.join("tuishou");
    let mut candidates = fs::read_dir(&state_dir)
        .unwrap()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("top-") && name.ends_with(".json"))
        })
        .collect::<Vec<_>>();
    assert_eq!(candidates.len(), 1, "expected one scoped top state file");
    candidates.pop().unwrap()
}

#[test]
fn swapping_keeps_children_alive_and_host_restores_terminal() {
    let dir = state_dir("swap");
    let script = dir.join("counter.sh");
    write_script(&script, counter_script());
    let reiki_pid = dir.join("reiki.pid");
    let lu_pid = dir.join("lu.pid");
    let reiki = format!("{} reiki {}", quoted(&script), quoted(&reiki_pid));
    let lu = format!("{} lu {}", quoted(&script), quoted(&lu_pid));
    let args = vec!["--reiki-cmd".to_string(), reiki, "--lu-cmd".to_string(), lu];
    let mut host = HostPty::start(&dir, &args);

    host.wait_for("reiki counter", |text| text.contains("FAKE reiki count=0"));
    host.send(b"+");
    host.wait_for("reiki increment", |text| {
        text.contains("FAKE reiki count=1")
    });

    host.send(&[0x1b, b'2']);
    host.wait_for("lu counter", |text| text.contains("FAKE lu count=0"));
    host.send(b"+");
    host.wait_for("lu increment", |text| text.contains("FAKE lu count=1"));

    host.send(&[0x1b, b'1']);
    host.wait_for("reiki state preserved", |text| {
        text.contains("FAKE reiki count=1")
    });

    let state_path = top_state_path(&dir);
    let state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    assert_eq!(state["active"], "reiki");
    assert_eq!(state["children"]["reiki"]["restart_count"], 0);
    assert_eq!(state["children"]["lu"]["restart_count"], 0);

    host.send(&[0x02, b'q']);
    host.wait_exit();
    let raw = host.raw();
    let raw_text = String::from_utf8_lossy(&raw);
    assert!(
        raw_text.contains("\u{1b}[?1049l"),
        "alternate screen not left"
    );
    assert!(raw_text.contains("\u{1b}[?25h"), "cursor not restored");
    assert!(!raw_text.contains("top-secret-token-do-not-log"));

    for pidfile in [reiki_pid, lu_pid] {
        let pid = fs::read_to_string(pidfile)
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap();
        assert!(process_gone(pid), "child {pid} survived host exit");
    }
    let state_text = fs::read_to_string(&state_path).unwrap();
    assert!(!state_text.contains("top-secret-token-do-not-log"));
}

#[test]
fn child_exit_is_reported_and_restart_works() {
    let dir = state_dir("exit");
    let counter = dir.join("counter.sh");
    write_script(&counter, counter_script());
    let quitter = dir.join("quitter.sh");
    write_script(
        &quitter,
        r#"#!/usr/bin/env bash
printf 'error: unrecognized subcommand top; build with the tui feature\r\n'
exit 2
"#,
    );
    let pidfile = dir.join("reiki.pid");
    let args = vec![
        "--reiki-cmd".to_string(),
        format!("{} reiki {}", quoted(&counter), quoted(&pidfile)),
        "--lu-cmd".to_string(),
        quoted(&quitter),
    ];
    let mut host = HostPty::start(&dir, &args);
    host.wait_for("lu exit report", |text| {
        text.contains("lu exited: status 2") && text.contains("tui")
    });
    let text = host.text();
    assert!(text.contains("lu!"), "no exit indicator in tab bar: {text}");

    host.send(&[0x1b, b'1']);
    host.wait_for("reiki counter", |text| text.contains("FAKE reiki count=0"));
    host.send(b"x");
    host.wait_for("reiki exit report", |text| {
        text.contains("reiki exited: status 7")
    });

    host.send(&[0x02, b'r']);
    host.wait_for("reiki restart", |text| {
        text.contains("FAKE reiki count=0") && text.contains("reiki restarted")
    });
    let state_path = top_state_path(&dir);
    let state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    assert_eq!(state["children"]["reiki"]["restart_count"], 1);
    host.send(&[0x02, b'q']);
    host.wait_exit();
}

#[test]
fn missing_binary_is_reported_with_install_hint() {
    let dir = state_dir("missing");
    let args = vec![
        "--reiki-cmd".to_string(),
        "/no/such/dir/reiki top".to_string(),
        "--lu-cmd".to_string(),
        "/no/such/dir/lu top".to_string(),
    ];
    let mut host = HostPty::start(&dir, &args);
    host.wait_for("missing binary report", |text| {
        text.contains("exited: failed to start") && text.contains("install a tui-enabled build")
    });
    host.send(&[0x02, b'q']);
    host.wait_exit();
}

#[cfg(unix)]
#[test]
fn signals_restore_terminal_and_stop_children() {
    for signal in [Signal::SIGTERM, Signal::SIGHUP] {
        let dir = state_dir(&format!("signal-{signal:?}"));
        let script = dir.join("counter.sh");
        write_script(&script, counter_script());
        let reiki_pid = dir.join("reiki.pid");
        let lu_pid = dir.join("lu.pid");
        let args = vec![
            "--reiki-cmd".to_string(),
            format!("{} reiki {}", quoted(&script), quoted(&reiki_pid)),
            "--lu-cmd".to_string(),
            format!("{} lu {}", quoted(&script), quoted(&lu_pid)),
        ];
        let mut host = HostPty::start(&dir, &args);
        host.wait_for("both children", |text| text.contains("FAKE reiki count=0"));
        let _ = kill(Pid::from_raw(i32::try_from(host.pid()).unwrap()), signal);
        host.wait_exit();
        let raw = host.raw();
        let raw_text = String::from_utf8_lossy(&raw);
        assert!(
            raw_text.contains("\u{1b}[?1049l"),
            "alternate screen not left on {signal:?}"
        );
        assert!(
            raw_text.contains("\u{1b}[?25h"),
            "cursor not restored on {signal:?}"
        );
        for pidfile in [reiki_pid, lu_pid] {
            let pid = fs::read_to_string(pidfile)
                .unwrap()
                .trim()
                .parse::<i32>()
                .unwrap();
            assert!(process_gone(pid), "child {pid} survived {signal:?}");
        }
    }
}

#[cfg(unix)]
fn process_gone(pid: i32) -> bool {
    let deadline = Instant::now() + WAIT;
    loop {
        match kill(Pid::from_raw(pid), None) {
            Err(nix::errno::Errno::ESRCH) => return true,
            _ if Instant::now() >= deadline => return false,
            _ => thread::sleep(Duration::from_millis(20)),
        }
    }
}

#[cfg(not(unix))]
fn process_gone(_pid: i32) -> bool {
    true
}
