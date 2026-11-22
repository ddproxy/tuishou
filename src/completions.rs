//! Small completion engine used by the generated shell scripts. Keeping the
//! candidate logic in the binary means the static scripts stay short and the
//! suggestions always match the current CLI.

pub const COMMANDS: &[&str] = &[
    "install",
    "update",
    "status",
    "rollback",
    "roll",
    "self-update",
    "auto-update",
    "adopt",
    "completions",
    "man",
];

const GLOBAL_OPTIONS: &[&str] = &["--json", "--quiet", "--verbose", "--help", "--version"];
const COMPONENTS: &[&str] = &["lu", "ting", "tuishou", "all"];
const AUTO_ACTIONS: &[&str] = &[
    "run",
    "install-timer",
    "status",
    "hold",
    "release",
    "clear-bad",
];

pub fn command_options(command: &str) -> &'static [&'static str] {
    match command {
        "install" => &[
            "--version",
            "--prefix",
            "--dry-run",
            "--allow-unattested",
            "--allow-major",
            "--smoke-check",
        ],
        "update" => &["--check", "--allow-major", "--allow-unattested", "--prefix"],
        "status" => &["--prefix"],
        "rollback" => &["--prefix"],
        "roll" => &[
            "--nodes",
            "--component",
            "--version",
            "--batch",
            "--dry-run",
            "--force-after",
            "--allow-major",
            "--allow-unattested",
        ],
        "self-update" => &[
            "--version",
            "--prefix",
            "--allow-unattested",
            "--allow-major",
        ],
        "auto-update" => &["--json", "--quiet"],
        "adopt" => &["--version", "--prefix"],
        "completions" => &[],
        "man" => &[],
        _ => &[],
    }
}

/// Return newline-separated candidates for the words typed so far. The last
/// word is the word currently being completed (it may be empty).
pub fn candidates(words: &[String]) -> String {
    let current = words.last().map(String::as_str).unwrap_or("");
    let mut pool: Vec<&str> = Vec::new();

    if current.starts_with('-') {
        pool.extend_from_slice(GLOBAL_OPTIONS);
        if let Some(first) = words.first() {
            if COMMANDS.contains(&first.as_str()) {
                pool.extend_from_slice(command_options(first));
            }
        }
    } else if words.len() <= 1 {
        pool.extend_from_slice(COMMANDS);
    } else if let Some(first) = words.first() {
        match first.as_str() {
            "install" | "update" | "rollback" | "adopt" => pool.extend_from_slice(COMPONENTS),
            "completions" => pool.extend_from_slice(&["bash", "zsh", "fish"]),
            "auto-update" if words.len() == 2 => pool.extend_from_slice(AUTO_ACTIONS),
            _ => {}
        }
        pool.extend_from_slice(GLOBAL_OPTIONS);
        pool.extend_from_slice(command_options(first));
    }

    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for item in pool {
        if item.starts_with(current) && seen.insert(item) {
            out.push(item);
        }
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn completes_top_level_commands() {
        let out = candidates(&words(&["in"]));
        assert_eq!(out, "install");
    }

    #[test]
    fn completes_shell_names() {
        let out = candidates(&words(&["completions", "z"]));
        assert_eq!(out, "zsh");
    }

    #[test]
    fn completes_flags_for_command() {
        let out = candidates(&words(&["install", "--dry"]));
        assert_eq!(out, "--dry-run");
    }
}
