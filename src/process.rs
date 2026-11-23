//! Thin child-process helper: argument vectors only, with a hard deadline,
//! bounded output and kill/reap on timeout. No shell is ever involved.

use crate::error::{Error, Result};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::{Duration, Instant};

pub(crate) const SAFE_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
pub(crate) const ENV_PASSTHROUGH: &[&str] = &[
    "HOME",
    "LANG",
    "LC_ALL",
    "TZ",
    "NO_COLOR",
    "XDG_RUNTIME_DIR",
];
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const READ_CHUNK: usize = 8192;

#[derive(Debug)]
pub struct CommandOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Run `program` with an argument vector. On deadline expiry the child's
/// process group is killed and the child is reaped, so a hung `lu`/`systemctl`
/// cannot wedge an update cycle.
pub fn run(program: &str, args: &[&str], timeout: Duration) -> Result<CommandOutput> {
    run_with_input(program, args, timeout, None)
}

/// Like [`run`], with an optional line written to the child's stdin.
pub fn run_with_input(
    program: &str,
    args: &[&str],
    timeout: Duration,
    input: Option<&str>,
) -> Result<CommandOutput> {
    let resolved = resolve_program(program)?;
    let mut command = Command::new(&resolved);
    command
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear()
        .env("PATH", SAFE_PATH);
    for key in ENV_PASSTHROUGH {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let mut child = command
        .spawn()
        .map_err(|e| Error::msg(format!("failed to start {program}: {e}")))?;
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        use std::io::Write;
        let _ = writeln!(stdin, "{input}");
    }

    let pgid = child.id();
    let (tx, rx) = mpsc::channel();
    if let Some(stdout) = child.stdout.take() {
        let tx = tx.clone();
        thread::spawn(move || capture(stdout, Stream::Out, tx));
    }
    if let Some(stderr) = child.stderr.take() {
        let tx = tx.clone();
        thread::spawn(move || capture(stderr, Stream::Err, tx));
    }
    drop(tx);

    let deadline = Instant::now() + timeout;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut out_done = false;
    let mut err_done = false;
    let mut status = None;
    loop {
        if status.is_none() {
            match child.try_wait() {
                Ok(done) => status = done,
                Err(e) => {
                    kill_and_reap(&mut child, pgid);
                    return Err(Error::msg(format!("failed to wait for {program}: {e}")));
                }
            }
        }
        if status.is_some() && out_done && err_done {
            break;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            kill_and_reap(&mut child, pgid);
            return Err(Error::msg(format!(
                "{program} timed out after {} ms and was killed",
                timeout.as_millis()
            )));
        }
        match rx.recv_timeout(remaining) {
            Ok(CaptureEvent::Chunk(Stream::Out, chunk)) => {
                stdout.extend_from_slice(&chunk);
                if stdout.len() > MAX_OUTPUT_BYTES {
                    kill_and_reap(&mut child, pgid);
                    return Err(Error::msg(format!(
                        "{program} stdout exceeded {MAX_OUTPUT_BYTES} bytes and was killed"
                    )));
                }
            }
            Ok(CaptureEvent::Chunk(Stream::Err, chunk)) => {
                stderr.extend_from_slice(&chunk);
                if stderr.len() > MAX_OUTPUT_BYTES {
                    kill_and_reap(&mut child, pgid);
                    return Err(Error::msg(format!(
                        "{program} stderr exceeded {MAX_OUTPUT_BYTES} bytes and was killed"
                    )));
                }
            }
            Ok(CaptureEvent::Done(Stream::Out)) => out_done = true,
            Ok(CaptureEvent::Done(Stream::Err)) => err_done = true,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                out_done = true;
                err_done = true;
            }
        }
    }

    let status = status.expect("child status after wait");
    Ok(CommandOutput {
        status: status.code().unwrap_or(-1),
        stdout: crate::http::redact_secrets(&String::from_utf8_lossy(&stdout)),
        stderr: crate::http::redact_secrets(&String::from_utf8_lossy(&stderr)),
    })
}

pub fn checked(program: &str, args: &[&str], timeout: Duration) -> Result<CommandOutput> {
    let output = run(program, args, timeout)?;
    if output.status == 0 {
        return Ok(output);
    }
    let detail = crate::logging::one_line(output.stderr.trim());
    let args = crate::http::redact_secrets(&args.join(" "));
    Err(Error::msg(format!(
        "{program} {args} failed with exit status {}{}",
        output.status,
        if detail.is_empty() {
            String::new()
        } else {
            format!(": {detail}")
        }
    )))
}

pub(crate) fn resolve_program(program: &str) -> Result<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_else(|| SAFE_PATH.into());
    resolve_program_with_path(program, &path)
}

/// Resolve a program against an explicit PATH and vet the resulting file.
/// Callers that spawn children use this instead of trusting inherited state.
pub(crate) fn resolve_program_with_path(program: &str, path: &std::ffi::OsStr) -> Result<PathBuf> {
    if program.is_empty() {
        return Err(Error::msg("empty program name"));
    }
    let candidate = if program.contains(std::path::MAIN_SEPARATOR) {
        PathBuf::from(program)
    } else {
        find_in_path(program, path)?
    };
    let absolute = if candidate.is_absolute() {
        candidate
    } else {
        std::env::current_dir()
            .map_err(|e| Error::msg(format!("failed to resolve {program}: {e}")))?
            .join(candidate)
    };
    validate_executable(&absolute)?;
    Ok(absolute)
}

fn find_in_path(program: &str, path: &std::ffi::OsStr) -> Result<PathBuf> {
    for dir in std::env::split_paths(path) {
        let dir = if dir.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            dir
        };
        let candidate = dir.join(program);
        if fs::symlink_metadata(&candidate).is_ok() {
            return Ok(candidate);
        }
    }
    Err(Error::msg(format!("{program} not found in PATH")))
}

pub(crate) fn validate_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = fs::symlink_metadata(path)
            .map_err(|e| Error::msg(format!("refusing {}: {e}", path.display())))?;
        let kind = meta.file_type();
        if kind.is_symlink() {
            // Distribution tools are often symlinks (alternatives, multicall coreutils). Accept
            // one only when the link itself is owned by root or the running user and its fully
            // resolved target passes the same checks as a plain file; the link path stays argv0.
            if let Some(uid) = current_uid() {
                let owner = meta.uid();
                if owner != uid && owner != 0 {
                    return Err(Error::msg(format!(
                        "refusing {}: symlink owned by uid {owner}",
                        path.display()
                    )));
                }
            }
            let target = fs::canonicalize(path)
                .map_err(|e| Error::msg(format!("refusing {}: {e}", path.display())))?;
            return validate_executable(&target);
        }
        if !kind.is_file() {
            return Err(Error::msg(format!(
                "refusing {}: not a regular file",
                path.display()
            )));
        }
        let mode = meta.mode();
        if mode & 0o022 != 0 {
            return Err(Error::msg(format!(
                "refusing {}: writable by group or others",
                path.display()
            )));
        }
        if let Some(uid) = current_uid() {
            let owner = meta.uid();
            if owner != uid && owner != 0 {
                return Err(Error::msg(format!(
                    "refusing {}: owner uid {owner} is neither the running user nor root",
                    path.display()
                )));
            }
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Effective UID from `/proc/self`, owned by the process user on Linux.
#[cfg(target_os = "linux")]
fn current_uid() -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    fs::metadata("/proc/self").ok().map(|m| m.uid())
}

#[cfg(not(target_os = "linux"))]
fn current_uid() -> Option<u32> {
    None
}

#[derive(Clone, Copy)]
enum Stream {
    Out,
    Err,
}

enum CaptureEvent {
    Chunk(Stream, Vec<u8>),
    Done(Stream),
}

fn capture<R: Read + Send + 'static>(mut reader: R, stream: Stream, tx: Sender<CaptureEvent>) {
    let mut buf = [0u8; READ_CHUNK];
    let mut total = 0usize;
    loop {
        let read = match reader.read(&mut buf) {
            Ok(read) => read,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        if read == 0 {
            break;
        }
        let allowed = MAX_OUTPUT_BYTES + 1 - total;
        let take = read.min(allowed);
        if take > 0
            && tx
                .send(CaptureEvent::Chunk(stream, buf[..take].to_vec()))
                .is_err()
        {
            return;
        }
        total += take;
        if total > MAX_OUTPUT_BYTES {
            break;
        }
    }
    let _ = tx.send(CaptureEvent::Done(stream));
}

#[cfg(unix)]
fn kill_and_reap(child: &mut Child, pgid: u32) {
    kill_group(pgid);
    let _ = child.kill();
    let _ = child.wait();
}

/// Signal the child's process group through the system `kill` utility so a
/// grandchild holding a pipe open is stopped even after the direct child exits.
#[cfg(unix)]
fn kill_group(pgid: u32) {
    if pgid < 2 {
        return;
    }
    let target = format!("-{pgid}");
    for path in ["/bin/kill", "/usr/bin/kill"] {
        let Ok(resolved) = fs::canonicalize(path) else {
            continue;
        };
        if validate_executable(&resolved).is_err() {
            continue;
        }
        let killed = Command::new(&resolved)
            .args(["-KILL", "--", &target])
            .env_clear()
            .env("PATH", SAFE_PATH)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok();
        if killed {
            return;
        }
    }
}

#[cfg(not(unix))]
fn kill_and_reap(child: &mut Child, _pgid: u32) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs;

    #[cfg(unix)]
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn helper(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    #[cfg(unix)]
    #[test]
    fn symlink_is_accepted_only_when_its_target_passes_the_same_checks() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("tool-real");
        fs::write(&target, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        let link = dir.path().join("tool");
        symlink(&target, &link).unwrap();
        validate_executable(&link).expect("symlink to a trusted file");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o775)).unwrap();
        let err = validate_executable(&link).unwrap_err().to_string();
        assert!(err.contains("writable"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn arguments_are_passed_verbatim() {
        let _guard = env_lock();
        let out = checked("printf", &["%s|%s", "a b", "c"], Duration::from_secs(2)).unwrap();
        assert_eq!(out.stdout.trim(), "a b|c");
    }

    #[cfg(unix)]
    #[test]
    fn timed_out_child_is_killed_and_reaped() {
        let _guard = env_lock();
        let start = Instant::now();
        let err = run("sleep", &["30"], Duration::from_millis(100)).unwrap_err();
        assert!(err.to_string().contains("timed out"));
        assert!(err.to_string().contains("killed"));
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[test]
    fn bare_program_resolves_through_path_and_env_is_minimal() {
        let _guard = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let script =
            "#!/bin/sh\nprintf '%s\\n' \"$PATH\"\nprintf '%s\\n' \"${TUISHOU_ENV_PROBE:-unset}\"\n";
        helper(dir.path(), "tuishou-path-helper", script);

        let old_path = env::var_os("PATH");
        let old_probe = env::var_os("TUISHOU_ENV_PROBE");
        env::set_var("PATH", dir.path());
        env::set_var("TUISHOU_ENV_PROBE", "inherited");
        let result = run("tuishou-path-helper", &[], Duration::from_secs(2));
        match old_path {
            Some(value) => env::set_var("PATH", value),
            None => env::remove_var("PATH"),
        }
        match old_probe {
            Some(value) => env::set_var("TUISHOU_ENV_PROBE", value),
            None => env::remove_var("TUISHOU_ENV_PROBE"),
        }

        let lines: Vec<_> = result.unwrap().stdout.lines().map(str::to_string).collect();
        assert_eq!(lines[0], SAFE_PATH);
        assert_eq!(lines[1], "unset");
    }

    #[cfg(unix)]
    #[test]
    fn writable_child_executable_is_refused() {
        let _guard = env_lock();
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = helper(dir.path(), "writable-helper", "#!/bin/sh\nexit 0\n");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o775)).unwrap();
        let err = run(&path.to_string_lossy(), &[], Duration::from_secs(2)).unwrap_err();
        assert!(err.to_string().contains("writable by group or others"));
    }

    #[cfg(unix)]
    #[test]
    fn oversized_output_is_refused() {
        let _guard = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let body = format!(
            "#!/bin/sh\nawk 'BEGIN {{ for (i = 0; i < {}; i++) printf \"x\" }}'\n",
            MAX_OUTPUT_BYTES + 1
        );
        let path = helper(dir.path(), "loud-helper", &body);
        let start = Instant::now();
        let err = run(&path.to_string_lossy(), &[], Duration::from_secs(10)).unwrap_err();
        assert!(err.to_string().contains("exceeded"));
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[cfg(unix)]
    #[test]
    fn descendant_holding_stdout_does_not_wedge_call() {
        let _guard = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let path = helper(
            dir.path(),
            "forking-helper",
            "#!/bin/sh\nsleep 30 &\nexit 0\n",
        );
        let start = Instant::now();
        let err = run(&path.to_string_lossy(), &[], Duration::from_millis(200)).unwrap_err();
        assert!(err.to_string().contains("timed out"));
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
