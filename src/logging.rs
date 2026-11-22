//! Single-line journal logging and notification dispatch.
//!
//! systemd's journal captures stdout/stderr; every event is emitted as one
//! line of `key=value` fields so `journalctl` and log collectors can parse it.
//! Multi-line command output is flattened before it is logged.

use crate::error::{Error, Result};
use serde::Serialize;
use std::io::Write;
use std::process::{Command, Stdio};

/// Flatten arbitrary text into a single journal-safe line.
pub fn one_line(text: &str) -> String {
    text.chars()
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
pub fn notify(argv: &[String], summary: &impl Serialize) -> Result<()> {
    if argv.is_empty() {
        return Ok(());
    }
    let json = serde_json::to_string(summary)
        .map_err(|e| Error::msg(format!("failed to serialize notification: {e}")))?;
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .arg(&json)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .map_err(|e| Error::msg(format!("notify command '{}' failed to start: {e}", argv[0])))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = writeln!(stdin, "{json}");
    }
    let status = child
        .wait()
        .map_err(|e| Error::msg(format!("notify command '{}' failed: {e}", argv[0])))?;
    if !status.success() {
        return Err(Error::msg(format!(
            "notify command '{}' exited with {status}",
            argv[0]
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
}
