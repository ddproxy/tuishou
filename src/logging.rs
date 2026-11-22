//! Single-line journal logging and notification dispatch.
//!
//! systemd's journal captures stdout/stderr; every event is emitted as one
//! line of `key=value` fields so `journalctl` and log collectors can parse it.
//! Multi-line command output is flattened before it is logged.

use crate::error::{Error, Result};
use serde::Serialize;
use std::time::Duration;

/// Flatten arbitrary text into a single journal-safe line. Known credentials
/// are removed before anything can be logged, returned to the CLI or sent to a
/// notification command.
pub fn one_line(text: &str) -> String {
    crate::http::redact_secrets(text)
        .chars()
        .map(|c| match c {
            '\n' | '\r' => ' ',
            '\t' => ' ',
            other => other,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote(value: &str) -> String {
    let flat = one_line(value);
    if flat.is_empty()
        || flat
            .bytes()
            .any(|b| b == b' ' || b == b'=' || b == b'"' || b == b'\'' || b == b'\\')
    {
        format!("\"{}\"", flat.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        flat
    }
}

/// Emit one structured line, e.g.
/// `tuishou: event=update component=lu version=v0.4.1 result=ok`.
pub fn journal(event: &str, fields: &[(&str, &str)]) {
    let mut line = format!("event={}", quote(event));
    for (key, value) in fields {
        line.push(' ');
        line.push_str(key);
        line.push('=');
        line.push_str(&quote(value));
    }
    eprintln!("tuishou: {line}");
}

pub fn journal_err(event: &str, fields: &[(&str, &str)]) {
    journal(event, fields);
}

/// Run a configured notify command with an argument vector. The JSON summary
/// is passed both as the final argument and on stdin; no shell is involved, so
/// argument vectors are never split or re-interpreted.
pub fn notify(argv: &[String], summary: &impl Serialize, timeout: Duration) -> Result<()> {
    if argv.is_empty() {
        return Ok(());
    }
    let json = serde_json::to_string(summary)
        .map_err(|e| Error::msg(format!("failed to serialize notification: {e}")))?;
    let args: Vec<&str> = argv[1..]
        .iter()
        .map(String::as_str)
        .chain([json.as_str()])
        .collect();
    let output = crate::process::run_with_input(&argv[0], &args, timeout, Some(&json))?;
    if output.status != 0 {
        return Err(Error::msg(format!(
            "notify command '{}' exited with status {}",
            argv[0], output.status
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_line_flattens_output() {
        assert_eq!(one_line("a\nb\t c"), "a b c");
    }

    #[test]
    fn one_line_redacts_known_tokens() {
        let _guard = crate::test_env_lock();
        let previous = std::env::var_os("GITHUB_TOKEN");
        std::env::set_var("GITHUB_TOKEN", "unit-test-secret");
        let line = one_line("gh failed with unit-test-secret");
        match previous {
            Some(value) => std::env::set_var("GITHUB_TOKEN", value),
            None => std::env::remove_var("GITHUB_TOKEN"),
        }
        assert_eq!(line, "gh failed with [redacted]");
    }
}
