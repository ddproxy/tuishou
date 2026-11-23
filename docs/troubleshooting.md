# Troubleshooting

For on-call users diagnosing an installer or `tuishou` failure.

## Exit codes

| Code | Source | Meaning |
| --- | --- | --- |
| `0` | installer and binary | Success. |
| `1` | binary | Error, including an attestation refusal. |
| `2` | installer and binary | Usage error, including an unknown component. |
| `3` | installer | Unsupported platform. |
| `4` | installer | Verification failed: checksum or attestation. |
| `5` | installer | Install failed: metadata, download, prefix or state. |
| `10` | binary | `tuishou update --check` found updates. |

The installer prints `ERROR:` on failures. The binary uses `attestation verification failed: ...`, `attestation refused: ...` and `attestation verification unavailable: ...` for verification errors.

## `ERROR: unsupported platform: <os>/<arch>`

The installer supports Linux x86_64 (`x86_64-unknown-linux-musl`) only. It prints the existing release targets and exits 3 before creating any directory. Fix: run on a supported host, or use the manual path in [install.md](install.md). Other targets are not claimed as supported by the installer.

## `checksum mismatch for <archive>: expected <digest>, got <digest>`

The downloaded archive does not match the `.sha256` asset. The installer exits 4 before installing or replacing any binary. Fix: retry the install; if the mismatch repeats, check the proxy or mirror serving the release assets. Do not bypass the checksum. A malformed checksum file reports `checksum file for <archive> is not a SHA-256 digest` and exits 4.

This is the checksum-only bootstrap: no attestation has been checked yet. See [security.md](security.md).

## `attestation: FAIL (tuishou verify <archive>)` / `attestation verification failed; the new binary was removed`

The binary installed from the archive could not verify that archive. The installer prints the `tuishou verify` output, removes the new binary, restores `tuishou.prev` when present, and exits 4. Fix: read the printed refusal, confirm the release has an attestation for the archive, check access to the GitHub API, and check `verify.signer_workflow` in `tuishou.toml` if it was changed from the default.

Reproduce directly:

```sh
tuishou verify <archive> --repo ddproxy/tuishou --tag <tag>
```

Component installs fail closed in the same way. `--allow-unattested` is an explicit local downgrade, not a fix; auto-update refuses it. Do not automate an install that reports this failure.

## `prefix <dir> is not writable by this user`

The install root cannot be written by the current user. The installer exits 5 and prints a root rerun command built from the original arguments. For a local script it has this shape:

```sh
sudo bash install.sh --prefix <dir> --yes
```

For the piped form it has this shape:

```sh
curl -fsSL https://raw.githubusercontent.com/ddproxy/tuishou/main/install.sh | sudo bash -s -- --prefix <dir> --yes
```

Use the exact command printed by the installer. Alternatively pass a writable `--prefix <dir>`; the default is `/usr/local` as root and `~/.local` otherwise.

## `release <tag> is missing <archive> or <checksum>`

The release metadata resolved, but the archive or `.sha256` asset for `x86_64-unknown-linux-musl` is absent. The installer exits 5. Fix: confirm the tag exists and carries both assets for this target, or select an existing tag with `--version <tag>`. Asset URLs must use https; loopback http is accepted for local test servers.

## `could not fetch release metadata for ddproxy/tuishou`

The release API request failed. The installer exits 5. Fix: check network access, GitHub API availability and rate limits, and the tag passed to `--version`. `TUISHOU_GITHUB_API_BASE` overrides the API base for release metadata and asset URLs; normal builds honor it, but a normal install does not need it.

## `unknown component '<name>'`

Known installer components are `reiki` and `lu`; `tuishou` is always installed. The installer exits 2. The binary uses the same wording for CLI component arguments and also accepts `tuishou` and `all`. Fix: correct the `--components` list or the command argument.

## `untrusted config <path>: must not be writable by group or others`

`tuishou.toml` is refused when group or other write bits are set. The binary also refuses a file owned by neither the running user nor root: `owner uid <uid> is neither the running user (<uid>) nor root`. Fix:

```sh
chmod go-w tuishou.toml
# change ownership only when the file is not owned by the running user or root
sudo chown <user> tuishou.toml
```

Unknown config keys, `require_attestation = false` and interval values below `5m` are also errors. See [configuration.md](configuration.md).

## Behavior that is easy to misread

- **No color:** set `NO_COLOR` to suppress installer ANSI color. Color is also omitted when stdout is not a terminal. The binary emits no ANSI color.
- **Dry run:** `bash install.sh --dry-run --prefix <dir>` resolves the tag and prints the plan without changes; checksum and attestation are not run. Use it to inspect the target before installing.
- **`~/.local/bin` not found:** a normal-user install lands in `~/.local/bin`. Add it to the shell path:

  ```sh
  export PATH="$HOME/.local/bin:$PATH"
  ```

  Root installs use `/usr/local/bin`, which is normally already on `PATH`.

- **Replace prompt:** with an existing destination and no `--yes`, the installer asks `Replace existing <path>? [y/N]`. Pass `--yes` to replace without the prompt; `--dry-run` never prompts.
- **`tuishou top`:** not implemented in this build. The installer lists it as planned in the next-commands block. Use `tuishou status`.
- **Auto-update timers:** `tuishou auto-update install-timer --interval 10m` only prints a systemd service and timer unit pair. Install and enable the units on the host; the command does not do it. Auto-update stays off until `[auto_update] enabled = true` is set and is not recommended for production.

## Where state lives

| Item | Location |
| --- | --- |
| Install state | `<prefix>/.tuishou.json`, else `$XDG_STATE_HOME/tuishou/installed.json` or `~/.local/state/tuishou/installed.json` |
| Auto-update state | `<prefix>/.tuishou/` (`auto.json`, `HOLD`, `lock`), else `$XDG_STATE_HOME/tuishou/` |
| Rollback copy | `<prefix>/bin/<name>.prev` |
| Verify cache | `<prefix>/verify-cache`, else the state parent plus `verify-cache` |

A missing `.prev` is an error, not a silent success. See [operations.md](operations.md) and [architecture.md](architecture.md).
