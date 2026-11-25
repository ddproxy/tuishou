//! Keep the public repository free of internal-looking identifiers.
//!
//! Patterns are assembled from fragments so the check can scan its own source.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const ALLOWLIST: &str = "tests/fixtures/repo-hygiene-allowlist.txt";

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn repository_files() -> Vec<PathBuf> {
    match git_tracked_files() {
        Some(files) => files,
        None => walk(root()),
    }
}

fn git_tracked_files() -> Option<Vec<PathBuf>> {
    let output = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(
        output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|part| !part.is_empty())
            .map(|part| root().join(String::from_utf8_lossy(part).as_ref()))
            .filter(|path| path.is_file())
            .collect(),
    )
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = fs::read_dir(&current)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", current.display()));
        for entry in entries {
            let entry = entry
                .unwrap_or_else(|e| panic!("cannot read an entry in {}: {e}", current.display()));
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if matches!(name.as_ref(), ".git" | "target" | "dist" | "artifacts") {
                    continue;
                }
                stack.push(path);
            } else if path.is_file()
                && name != ".DS_Store"
                && name != ".env"
                && !name.ends_with(".log")
            {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

fn allowlist() -> BTreeSet<(String, String)> {
    let text = fs::read_to_string(root().join(ALLOWLIST)).expect("read hygiene allowlist");
    let mut entries = BTreeSet::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(3, |c: char| c.is_whitespace());
        let category = parts.next().expect("allowlist category");
        let path = parts.next().expect("allowlist path");
        let reason = parts.next().unwrap_or_default().trim();
        assert!(!reason.is_empty(), "allowlist entry needs a reason: {line}");
        assert!(
            root().join(path).is_file(),
            "allowlist names a missing file: {path}"
        );
        entries.insert((category.to_string(), path.to_string()));
    }
    entries
}

fn internal_node_name(line: &str) -> bool {
    let needle = format!("{}-{}-", "dev", "ct");
    line.to_ascii_lowercase().contains(&needle)
}

fn internal_domain(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    for tld in [
        "internal",
        "corp",
        "intranet",
        "localdomain",
        "lan",
        "home.arpa",
    ] {
        let suffix = format!(".{tld}");
        let mut offset = 0;
        while let Some(index) = lower[offset..].find(&suffix) {
            let start = offset + index + suffix.len();
            let following = lower.as_bytes().get(start).copied();
            if following.is_none_or(|byte| !byte.is_ascii_alphanumeric() && byte != b'-') {
                return true;
            }
            offset = start;
        }
    }
    lower.contains(&format!("{}.", "internal"))
}

fn parse_ipv4(token: &str) -> Option<[u8; 4]> {
    let parts: Vec<_> = token.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut octets = [0_u8; 4];
    for (slot, part) in parts.iter().enumerate() {
        octets[slot] = part.parse().ok()?;
    }
    Some(octets)
}

fn private_address(line: &str) -> bool {
    let mut token = String::new();
    for character in line.chars().chain(std::iter::once(' ')) {
        if character.is_ascii_digit() || character == '.' {
            token.push(character);
            continue;
        }
        if let Some([first, second, _, _]) = parse_ipv4(&token) {
            let private = first == 10
                || first == 172 && (16..=31).contains(&second)
                || first == 192 && second == 168
                || first == 169 && second == 254
                || first == 100 && (64..=127).contains(&second);
            if private {
                return true;
            }
        }
        token.clear();
    }
    false
}

fn key_owner_handle(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    let keyword = format!("{}{}", "key", "owner");
    if !lower.contains(&keyword) {
        return false;
    }
    for (index, byte) in lower.as_bytes().iter().enumerate() {
        if *byte != b'@' {
            continue;
        }
        let rest = &lower[index + 1..];
        let length = rest
            .chars()
            .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_' || *c == '-')
            .count();
        if length < 5 {
            continue;
        }
        let following = rest.as_bytes().get(length).copied();
        if following.is_none_or(|next| next != b':' && next != b'/' && next != b'.') {
            return true;
        }
    }
    false
}

#[test]
fn detectors_flag_forbidden_identifiers() {
    assert!(internal_node_name(&format!(
        "node = \"{}-{}-9\"",
        "dev", "ct"
    )));
    assert!(internal_domain(&format!(
        "https://host.{}/health",
        "internal"
    )));
    assert!(internal_domain(&format!(
        "https://{}.example/health",
        "internal"
    )));
    assert!(private_address(&format!("{}.{}.{}.{}", 10, 1, 2, 3)));
    assert!(key_owner_handle(&format!(
        "\"x-{}{}\": \"@{}\"",
        "key", "owner", "exampleowner"
    )));

    assert!(!internal_domain("https://node.example/health"));
    assert!(!private_address("http://127.0.0.1:8080"));
    assert!(!private_address("http://192.0.2.1"));
}

#[test]
fn tracked_files_are_free_of_internal_identifiers() {
    let allowed = allowlist();
    let mut findings = Vec::new();
    for path in repository_files() {
        let relative = path
            .strip_prefix(root())
            .expect("file under the repository root")
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        for (number, line) in text.lines().enumerate() {
            for (category, found) in [
                ("host", internal_node_name(line)),
                ("domain", internal_domain(line)),
                ("address", private_address(line)),
                ("handle", key_owner_handle(line)),
            ] {
                if found && !allowed.contains(&(category.to_string(), relative.clone())) {
                    findings.push(format!("{relative}:{}: {category}", number + 1));
                }
            }
        }
    }
    assert!(
        findings.is_empty(),
        "internal identifiers in repository files:\n{}",
        findings.join("\n")
    );
}
