// Static shell-completion and man-page sources. These are shared by the CLI
// (`tuishou completions` / `tuishou man`) and by the build script, which
// materialises them into `dist/` at build time.

pub const BASH_COMPLETIONS: &str = r#"# bash completion for tuishou
_tuishou() {
    local cur="${COMP_WORDS[COMP_CWORD]}"
    local words=("${COMP_WORDS[@]:1}")
    COMPREPLY=($(compgen -W "$(tuishou __complete -- "${words[@]}")" -- "$cur"))
}
complete -F _tuishou tuishou
"#;

pub const ZSH_COMPLETIONS: &str = r#"#compdef tuishou
_tuishou() {
    local -a replies
    replies=(${(f)"$(tuishou __complete -- "${words[@]:1}")"})
    compadd -a replies
}
compdef _tuishou tuishou
"#;

pub const FISH_COMPLETIONS: &str = r#"# fish completion for tuishou
function __tuishou_complete
    set -l tokens (commandline -opc)
    set -e tokens[1]
    set -l cur (commandline -ct)
    tuishou __complete -- $tokens $cur
end
complete -c tuishou -f -a '(__tuishou_complete)'
"#;

pub const MAN_PAGE: &str = r#".TH TUISHOU 1 "tuishou" "tuishou" "User Commands"
.SH NAME
tuishou \- install, update and roll the Tuishou stack from GitHub releases
.SH SYNOPSIS
.B tuishou
[\fIglobal options\fR]
.I command
[\fIoptions\fR]
.SH DESCRIPTION
.B tuishou
downloads release archives, verifies SHA-256 checksums and (when \fBgh\fR is
available) build attestations, then installs or updates binaries atomically
while keeping the previous binary as \fI<name>.prev\fR.
.SH COMMANDS
.TP
.B install <component|all>
Install a component; supports \fB--version\fR, \fB--prefix\fR, \fB--dry-run\fR,
\fB--allow-unattested\fR and \fB--allow-major\fR.
.TP
.B update [component|all]
Update installed components; \fB--check\fR exits 10 when updates exist.
.TP
.B status
Show installed and latest versions.
.TP
.B rollback <component>
Restore the previous binary for a component.
.TP
.B roll --nodes <file|list>
Run a controlled fleet roll over SSH.
.TP
.B self-update
Update the \fBtuishou\fR binary itself from its own release archive.
.TP
.B auto-update <run|install-timer|status|hold|release|clear-bad>
Opt-in scheduled auto-update. \fBrun\fR performs one cycle under an exclusive
flock; \fBinstall-timer\fR prints (never installs) a systemd service/timer pair;
\fBstatus\fR reports the last check and per-component results; \fBhold\fR and
\fBrelease\fR manage the HOLD file; \fBclear-bad\fR forgets bad versions.
This mode is off by default and is not recommended for production: it installs
new releases without human review.
.TP
.B adopt <component> --version <tag>
Record an existing binary in state without replacing it.
.TP
.B completions <bash|zsh|fish>
Print a shell completion script to stdout.
.TP
.B man
Print this man page to stdout.
.SH GLOBAL OPTIONS
.TP
.B --json
Output JSON where supported.
.TP
.B --quiet
Suppress non-essential output.
.TP
.B --verbose
Print verbose output.
.SH FILES
.TP
.I $XDG_STATE_HOME/tuishou/installed.json
Default install state (or \fI<prefix>/.tuishou.json\fR with \fB--prefix\fR).
.TP
.I $XDG_STATE_HOME/tuishou/
Auto-update state (HOLD, lock, ETag cache, results); directory mode 0700.
With \fB--prefix\fR this is \fI<prefix>/.tuishou\fR instead.
.TP
.I dist/
Generated completions and man page; not committed to source control.
.SH SEE ALSO
.BR gh-attestation-verify (1),
.BR sha256sum (1)
"#;

pub const DIST_FILES: &[(&str, &str)] = &[
    ("tuishou.bash", BASH_COMPLETIONS),
    ("tuishou.zsh", ZSH_COMPLETIONS),
    ("tuishou.fish", FISH_COMPLETIONS),
    ("tuishou.1", MAN_PAGE),
];
