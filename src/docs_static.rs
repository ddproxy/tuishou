// Static shell-completion and man-page sources. These are shared by the CLI
// (`tuishou completions` / `tuishou man`) and by the build script, which
// materialises them under OUT_DIR at build time.

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
tuishou \- install and update the Tuishou stack from GitHub releases
.SH SYNOPSIS
.B tuishou
[\fIglobal options\fR]
.I command
[\fIoptions\fR]
.SH DESCRIPTION
.B tuishou
downloads release archives, verifies SHA-256 checksums and in-process Sigstore
attestations, then installs or updates binaries atomically while keeping the
previous binary as \fI<name>.prev\fR.
.PP
The built-in components are \fBreiki\fR (ddproxy/reiki), \fBlu\fR
(ddproxy/tuishou-lu) and \fBtuishou\fR (ddproxy/tuishou), in that report order.
See \fBdocs/components.md\fR for assets, binaries, units and health checks.
.SH COMMANDS
.TP
.B install <component|all>
Install a component; supports \fB--version\fR, \fB--prefix\fR, \fB--dry-run\fR,
\fB--allow-unattested\fR and \fB--allow-major\fR.
.TP
.B update [component|all]
Update components; a bare invocation means \fBall\fR. An unknown name is a usage
error listing the known names. \fB--check\fR exits 0 when nothing installable is
pending, 10 when an installed component has an installable update, 1 on an
evaluation error and 2 for usage errors.
.TP
.B status
Show one status line per built-in component in table order. Each line uses one
fixed phrase: \fBup to date (vX)\fR, \fBupdate available vA -> vB\fR,
\fBupdated vA -> vB\fR, \fBnot installed (latest vB)\fR,
\fBnot in this release (archive lacks: <binaries>)\fR, \fBheld (reason)\fR,
\fBbad version skipped (vX)\fR, \fBskipped (reason)\fR or \fBfailed (reason)\fR.
Each line also carries \fBinstalled=\fR, \fBlatest=\fR and \fBlast_check=\fR fields;
the check time comes from auto-update state and falls back to the cycle check time
or \fBnever\fR. \fB--json\fR carries the same data.
.TP
.B rollback <component>
Restore the previous binary for a component.
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
Every \fBauto-update\fR action accepts \fB--prefix\fR, which overrides
\fB[auto_update].prefix\fR for state, binaries and the generated service.
\fBinstall-timer\fR uses the configured \fBinterval\fR unless \fB--interval\fR
is given; both are subject to the 5m minimum.
.TP
.B top [\fB--reiki-cmd\fR \fIARGV\fR] [\fB--lu-cmd\fR \fIARGV\fR]
Host \fBreiki top\fR and \fBlu top\fR in one terminal. Both children start at
launch and keep running, each in its own pseudo-terminal with a vt100 screen;
only the active one is drawn, so selection, scroll position and filters survive
a swap. Resizes are forwarded to both children. Requires the \fBtop\fR feature,
which is on by default in release builds; minimal builds can drop it with
\fBcargo build --no-default-features\fR.
.IP
Keys: \fBAlt+1\fR or \fBF1\fR selects Reiki, \fBAlt+2\fR or \fBF2\fR selects Lu,
\fBCtrl+Tab\fR cycles. Everything else goes to the active child. The prefix
\fBCtrl+b\fR opens host commands: \fBr\fR restarts the active child, \fBq\fR
quits all, \fB?\fR shows help, and \fBEsc\fR cancels. Mouse events are passed
through only while the active child requests mouse tracking.
.IP
Each \fIARGV\fR is a quoted argument string, not a shell command: quotes group
arguments and nothing is expanded. \fB[top] reiki_cmd\fR and \fB[top] lu_cmd\fR
in \fItuishou.toml\fR accept the same argv as arrays. Program defaults come
from the component registry; tokens should be supplied through the child's own
environment variables, never on the command line. The active tab, restart
counts and last arguments are recorded under the state directory.
.TP
.B adopt <component> --version <tag>
Record an existing binary in state without replacing it.
.TP
.B completions <bash|zsh|fish>
Print a shell completion script to stdout.
.TP
.B man
Print this man page to stdout.
.TP
.B verify \fIfile\fR --repo \fIowner/name\fR [\fB--signer-workflow\fR \fIpath\fR] [\fB--tag\fR \fItag\fR]
Verify a local artifact against its GitHub attestation bundle. Prints the
subject digest, OIDC issuer, workflow identity, expected workflow path, ref and
Rekor log index. Exit status is 0 when verified, 1 when refused and 2 for usage
errors.
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
.TP
.B --config \fIpath\fR
Load \fIpath\fR instead of \fI./tuishou.toml\fR. The environment variable
\fBTUISHOU_CONFIG\fR sets the same value; the command-line flag wins.
Generated systemd units always pass an absolute configuration path.
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
.TP
.I $XDG_STATE_HOME/tuishou/top.json
Host TUI state: last active tab, per-child restart count and last arguments
(mode 0600). With \fB--prefix\fR this is \fI<prefix>/.tuishou-top.json\fR.
.TP
.I $XDG_STATE_HOME/tuishou/verify-cache/
Trusted-root cache (mode 0700); override with \fB[verify].cache_dir\fR.
.SH SEE ALSO
.BR sha256sum (1)
"#;

pub const DIST_FILES: &[(&str, &str)] = &[
    ("tuishou.bash", BASH_COMPLETIONS),
    ("tuishou.zsh", ZSH_COMPLETIONS),
    ("tuishou.fish", FISH_COMPLETIONS),
    ("tuishou.1", MAN_PAGE),
];
