//! Thin child-process helper: argument vectors only, with a hard deadline and
//! kill/reap on timeout. No shell is ever involved.

use crate::error::{Error, Result};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct CommandOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Run `program` with an argument vector. On deadline expiry the child is
/// killed and reaped, so a hung `lu`/`systemctl` cannot wedge an update cycle.
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
    let mut child = Command::new(program)
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::msg(format!("failed to start {program}: {e}")))?;
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        use std::io::Write;
        let _ = writeln!(stdin, "{input}");
    }

    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(Error::msg(format!(
                        "{program} timed out after {} ms and was killed",
                        timeout.as_millis()
                    )));
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::msg(format!("failed to wait for {program}: {e}")));
            }
        }
    }

    let output = child
        .wait_with_output()
        .map_err(|e| Error::msg(format!("failed to read {program} output: {e}")))?;
    Ok(CommandOutput {
        status: output.status.code().unwrap_or(-1),
        stdout: crate::http::redact_secrets(&String::from_utf8_lossy(&output.stdout)),
        stderr: crate::http::redact_secrets(&String::from_utf8_lossy(&output.stderr)),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn arguments_are_passed_verbatim() {
        let out = checked("printf", &["%s|%s", "a b", "c"], Duration::from_secs(2)).unwrap();
        assert_eq!(out.stdout.trim(), "a b|c");
    }

    #[cfg(unix)]
    #[test]
    fn timed_out_child_is_killed_and_reaped() {
        let start = Instant::now();
        let err = run("sleep", &["30"], Duration::from_millis(100)).unwrap_err();
        assert!(err.to_string().contains("timed out"));
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
