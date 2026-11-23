# CLI reference

For script authors driving `tuishou` from shells and CI.

Use `tuishou [GLOBAL FLAGS] <COMMAND> [FLAGS]`. The built-in component names are `reiki`, `lu` and `tuishou`. `ting` is a binary in the `lu` archive, not a component.

## Global flags

| Flag | Default | Meaning |
| --- | --- | --- |
| `--json` | off | JSON output where supported. |
| `--quiet` | off | Suppress non-essential output. |
| `--verbose` | off | Verbose output. |
| `--config <path>` | `./tuishou.toml` | Config file. `TUISHOU_CONFIG` sets the same value; the flag wins. An explicit missing file is an error; the default path may be absent. |

## Commands

### `install <component|all>`

Install from the component's built-in GitHub release. Flags:

| Flag | Default | Meaning |
| --- | --- | --- |
| `--version <tag>` | latest stable | Release tag; a leading `v` is added when missing. |
| `--prefix <dir>` | `$HOME/.local/bin`, else `/usr/local/bin` | Binary directory for this command, not the installer root. |
| `--dry-run` | off | Resolve the release and print the plan; make no changes. |
| `--allow-unattested` | off | Warn and continue when attestation verification fails. Checksums are still enforced. |
| `--allow-major` | off | Permit crossing a major version boundary. |
| `--smoke-check` | off | Run `--version` on staged binaries before commit. A non-zero exit or empty output fails the install. |

`reiki` and `lu` use local apply paths that check configuration, drain and health before committing state. All component installs verify attestations in process.

### `update [component|all]`

Update installed components. A bare command targets every built-in component but does not install missing ones; `update <name>` and `update all` can install a missing component. Flags:

| Flag | Default | Meaning |
| --- | --- | --- |
| `--check` | off | Report without installing. Exit `10` when an installable update exists, `1` on a failed evaluation, `0` when current. |
| `--allow-major` | off | Permit crossing a major version boundary. |
| `--allow-unattested` | off | Warn and continue when attestation verification fails; never affects checksums. |
| `--prefix <dir>` | default binary directory | Binary directory. Holds and bad versions are read from the same prefix or the configured auto-update prefix. |

Holds and bad versions from auto-update state are considered. A configured fleet gate can skip a cycle and still exit `0`.

### `status [--prefix <dir>]`

Print one fixed-vocabulary line per component. `--json` gives the same data in machine-readable form.

### `rollback <component> [--prefix <dir>]`

Restore `<name>.prev` in the binary directory. A missing `.prev` is an error. `--json` prints `component` and `rolled_back`.

### `self-update [FLAGS]`

Update the `tuishou` binary from its own release archive. Flags:

| Flag | Default | Meaning |
| --- | --- | --- |
| `--version <tag>` | latest stable | Release tag. |
| `--prefix <dir>` | directory of the running binary | Destination directory. |
| `--allow-unattested` | off | Warn and continue on attestation failure; never affects checksums. |
| `--allow-major` | off | Permit crossing a major version boundary. |
| `--dry-run` | off | Print the plan; make no changes. |

The staged binary always receives a `--version` check before commit. `--json` prints `component`, `version`, `binaries`, `dry_run` and `self`.

### `completions <bash|zsh|fish>`

Print the completion script for the chosen shell to stdout.

### `man`

Print the generated man page to stdout.

### `verify <file> --repo <owner/name> [FLAGS]`

Verify a local artifact against its GitHub attestation bundle. Flags:

| Flag | Default | Meaning |
| --- | --- | --- |
| `--repo <owner/name>` | required | Repository that produced the artifact. |
| `--signer-workflow <path>` | `[verify].signer_workflow` | Expected signer workflow; the config default is `.github/workflows/release.yml`. |
| `--tag <tag>` | required | Expected release tag; it must be a tag ref and is resolved to a source commit digest. |

`--json` prints `digest`, `issuer`, `identity`, `workflow`, `ref`, `log_index` and `predicate_type`. Text output prints all fields except `predicate_type`.

### `adopt <component> --version <tag> [--prefix <dir>]`

Record an existing binary in state without replacing it. At least one expected binary must be present in the binary directory. The prefix comes from the flag, then `[auto_update].prefix`, then the default binary directory.

### `auto-update <SUBCOMMAND>`

Opt-in scheduled updates. Requires `[auto_update] enabled = true`. Every subcommand accepts `--prefix`, which overrides `[auto_update].prefix`.

| Subcommand | Flags | Behavior |
| --- | --- | --- |
| `run` | `--once`, `--allow-unattested`, `--prefix` | One cycle under an exclusive lock; no daemon. `--once` is an accepted marker for systemd unit clarity. `--allow-unattested` is always refused. |
| `install-timer` | `--interval <duration>`, `--prefix` | Print, but do not install, a systemd service and timer pair. Interval defaults to `[auto_update].interval` (`10m`) and has a `5m` minimum. Units include `OnBootSec` and `OnActiveSec`. |
| `status` | `--prefix` | Show last check, per-component results, hold state and next version. |
| `hold` | `--reason <text>`, `--expires-at <RFC3339>`, `--expires-in <duration>`, `--prefix` | Create the `HOLD` file. |
| `release` | `--prefix` | Remove the `HOLD` file. |
| `clear-bad [<component>]` | `--prefix` | Forget bad versions for one component or all components. |

### `tuishou __complete <words...>`

Hidden completion hook used by the generated scripts. It runs before config loading so completion still works with a broken config. Generated scripts pass `--` followed by the typed words.

### `tuishou top [FLAGS]`

Host `reiki top` and `lu top` in one terminal. The `top` feature is enabled by default; a build without it exits with a usage error explaining how to rebuild.

| Flag | Default | Meaning |
| --- | --- | --- |
| `--reiki-cmd <ARGV>` | component registry | Override the reiki child argv. |
| `--lu-cmd <ARGV>` | component registry | Override the lu child argv. |

Override strings are argument vectors, not shell commands: quotes group arguments, backslash escapes the next character, and nothing is expanded. A CLI flag wins over `[top]` config. Saved argv from state is never executed. Children are resolved through the safe program resolver and started with a cleared environment plus a small allowlist; `GITHUB_TOKEN` and `GH_TOKEN` are not inherited. TUI state is scoped to the config file and install prefix and is read only after ownership, mode and symlink checks. See [top.md](top.md).

## Exit codes

Binary exit codes:

| Code | Meaning |
| --- | --- |
| `0` | Success. |
| `1` | Error, including verification refusal. |
| `2` | Usage error, including unknown components and invalid `verify` arguments. |
| `10` | Updates available from `update --check`. |

Installer exit codes are separate:

| Code | Meaning |
| --- | --- |
| `0` | OK. |
| `2` | Usage. |
| `3` | Unsupported platform. |
| `4` | Verification failed: checksum or attestation. |
| `5` | Install failed. |

## Attestation downgrade

`--allow-unattested` exists only on `install`, `update` and `self-update`. It downgrades attestation failures after the SHA-256 checksum has passed; it never skips or downgrades checksums. Auto-update refuses the flag, and `[auto_update] require_attestation = false` is a config error.

## Environment

| Variable | Used by | Meaning |
| --- | --- | --- |
| `GITHUB_TOKEN`, `GH_TOKEN` | Binary | Token sources, first non-empty. Sent only to GitHub API endpoints, stripped on redirects and redacted from errors. Asset downloads carry no token. |
| `TUISHOU_CONFIG` | Binary | Same as `--config`; the flag wins. |
| `TUISHOU_GITHUB_API_BASE` | Installer and binary | Overrides `https://api.github.com`. The binary uses it for release metadata, source-digest resolution and attestation API calls; test fixtures point it at a local fake release server. |
| `TUISHOU_NODE_NAME` | Binary apply paths | Node identity when `[auto_update].node_name` is unset; falls back to `HOSTNAME`, then `localhost`. |
| `NO_COLOR` | Installer and binary | The installer suppresses ANSI styling when set, and uses styling only on a terminal. The binary emits no ANSI color in any case. |
| `TUISHOU_MAX_DOWNLOAD_BYTES` | Binary | Overrides the `200 * 1024 * 1024` byte download cap; invalid values fall back to the default. |
| `TUISHOU_TIMER_EXE` | Binary | Executable path written into printed systemd units; default `/usr/local/bin/tuishou`. Overrides must be existing regular files in a trusted directory: `..` components are rejected, the path is canonicalized, and owner and mode are checked. |

Test-only hooks are ignored by normal release binaries because they are gated behind the `test-hooks` feature:

| Variable | Test-only meaning |
| --- | --- |
| `TUISHOU_FAKE_ATTESTATION` | Select the fail-closed fake verifier. |
| `TUISHOU_ALLOWED_HOSTS` | Add hosts to the redirect allowlist. |
| `TUISHOU_ALLOW_INSECURE_HTTP` | Permit loopback HTTP for fake servers. |

`TUISHOU_GITHUB_API_BASE` is not test-only; normal builds honor it. See [install](install.md) for installer options and [architecture](architecture.md) for the release flow.
