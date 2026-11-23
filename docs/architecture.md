# Architecture

For contributors changing release, install, state or auto-update code.

## Bootstrap and install flow

1. `install.sh` resolves a latest or tagged release through `TUISHOU_GITHUB_API_BASE` (default `https://api.github.com`) and downloads `tuishou-<tag>-<target>.tar.gz` plus its `.sha256` asset. The shell installer bootstraps only Linux x86_64.
2. The shell checks SHA-256 before executing release code, extracts the top-level `tuishou` into a private stage under `<installer prefix>/.tuishou-install.XXXXXX`, stages it into `<prefix>/bin`, and keeps `tuishou.prev` when replacing an existing binary.
3. The staged binary runs `tuishou verify <archive> --repo ddproxy/tuishou --tag <tag>`. Failure restores the previous binary or removes the new one and exits `4`. Success runs `tuishou adopt tuishou --version <tag> --prefix <prefix>/bin`, then `tuishou install <component> --prefix <prefix>/bin` for each requested component. The installer `--prefix` is an install root; the binary `--prefix` is a binary directory.
4. Install and update reuse one component path. `src/cli.rs` loads the config and resolves target names. `src/install.rs` selects the release, detects the target in `src/target.rs`, rejects major-version crossings unless allowed, and downloads into a private temporary directory.
5. The SHA-256 from the `.sha256` asset is always verified. A source digest is resolved from release metadata when available, then `src/verify/mod.rs` fetches bundles through `src/verify/github.rs` and verifies them in process with the `sigstore` crate and the pinned or operator-supplied trust roots in `src/verify/trust.rs` and `assets/`. No `gh` subprocess is used. `--allow-unattested` downgrades only the attestation step, never checksums, and auto-update refuses it.
6. Required binaries are extracted. Reiki receives a staged config check; optional staged `--version` smoke checks run before commit. `commit_binaries` copies destinations and `.prev` files to unique temporary files, moves staged binaries into place, then replaces `.prev` only after every new binary is committed. On failure it restores `.prev` first, then the installed binaries.
7. State is written after commit. Reiki and Lu apply paths defer state until configuration, drain and health checks pass. `tuishou rollback <component>` copies `<name>.prev` back and refreshes that copy.

## Files and state

| Item | Path | Notes |
| --- | --- | --- |
| Config | `./tuishou.toml`, or `--config` / `TUISHOU_CONFIG` | Keys and defaults in `src/config.rs`. Unknown keys are errors; group- or other-writable files are refused. |
| Binary directory | `--prefix` when set, else `$HOME/.local/bin`, else `/usr/local/bin` | `install_dir()` in `src/install.rs`. |
| Rollback copy | `<binary directory>/<name>.prev` | One copy per binary; the installer also keeps `tuishou.prev`. |
| Install state | `<prefix>/.tuishou.json`; else `$XDG_STATE_HOME/tuishou/installed.json`; else `$HOME/.local/state/tuishou/installed.json`; else `./.tuishou.json` | Mode `0600`, written atomically. Holds version, target, checksum, install time and previous version. |
| Auto-update state directory | `<prefix>/.tuishou`; else `$XDG_STATE_HOME/tuishou`; else `$HOME/.local/state/tuishou`; else `./.tuishou` | Created with mode `0700`. |
| Auto-update state | `<auto-update state directory>/auto.json` | Last check, per-component results, bad versions, failure counts and release caches. |
| Hold and lock | `<auto-update state directory>/HOLD`, `<auto-update state directory>/lock` | `HOLD` is JSON or plain-text reason; expired holds are removed. `lock` is an advisory OS lock released by the kernel. |
| Verify cache | `[verify].cache_dir`; else `<prefix>/verify-cache` or the state parent plus `verify-cache`; cwd fallback `.tuishou-verify-cache` | Trusted-root cache; `0700`. |
| Installer stage | `<installer prefix>/.tuishou-install.XXXXXX` | Removed on exit. |

State writes use unique temporary files, `fsync` and rename. Auto-update state changes run while holding the cycle lock.

## Components and assets

The table in `src/component.rs` sets report order and built-in release metadata.

| Component | Repository | Asset pattern (plus `.sha256`) | Required binaries | Notes |
| --- | --- | --- | --- | --- |
| `reiki` | `ddproxy/reiki` | `reiki-{tag}-{target}.tar.gz` | `reiki` | Config check at `/var/lib/reiki/reiki.toml`; service unit `reiki.service`. |
| `lu` | `ddproxy/tuishou-lu` | `lu-{tag}-{target}.tar.gz` | `lu`, `ting` | Both binaries are installed and rolled back together; `ting` is not a component. |
| `tuishou` | `ddproxy/tuishou` | `tuishou-{tag}-{target}.tar.gz` | `tuishou` | Source for `self-update` and the installer bootstrap. |

`{tag}`, `{target}` and `{name}` are substituted from the release and detected target. A checksum asset is the archive name plus `.sha256`. `binary_ranges` can change expected binaries per release. Required binaries are checked against archive contents before install; an archive missing any required binary is reported as not in that release instead of being partially installed. Config overrides may change binaries, ranges, paths and policy, but never `repo` or `asset_pattern`. The expected signer workflow defaults to `.github/workflows/release.yml` with per-component overrides under `[verify]`.

## Release targets

`src/target.rs` lists four target strings. Only the two Linux musl targets are marked verified; other listed targets produce a warning unless output is quiet.

| Target string | Support |
| --- | --- |
| `x86_64-unknown-linux-musl` | Detected on Linux x86_64; verified; installer bootstrap; release workflow builds it. |
| `aarch64-unknown-linux-musl` | Detected on Linux aarch64; verified; release workflow builds it only when a musl cross-linker is configured. |
| `x86_64-apple-darwin` | Listed but unverified; not built by the release workflow. |
| `aarch64-apple-darwin` | Listed but unverified; not built by the release workflow. |

Unsupported host and architecture pairs return an error. The shell installer exits `3` for anything except Linux x86_64 and points to the manual install path.

## Auto-update

Auto-update is opt-in: `[auto_update] enabled = true` and per-component `[[component]] auto_update = true` are both required. It accepts stable releases only, rejects `require_attestation = false`, and refuses `--allow-unattested`. There is no daemon; a systemd timer or cron starts `tuishou auto-update run --once`, and a second concurrent cycle exits quietly under the advisory lock.

Eligibility uses the release cache in `auto.json`, version policy, minimum release age, bad versions, holds and controller gating. Reiki is applied before the Lu agent in the same cycle. Holds pause updates; `clear-bad` allows a rejected version to be tried again.

`tuishou auto-update install-timer` prints a service and timer unit pair with an absolute config path, `OnBootSec`, `OnActiveSec`, jitter and `Persistent=true`. It does not write, enable or start units. The operator installs the printed units.

## Tests and fixtures

- `make test` runs workflow validation, the installer test and the Rust suite with the `test-hooks` feature.
- Installer changes use `tests/fixtures/installer/fake_release_server.sh` and `fake-tuishou.sh` through `tests/installer_test.sh`. Run `make installer-test`. Tests must not need live GitHub.
- `make docs-check` runs `scripts/docs-check.sh` after a build. It executes the quickstart commands, validates configuration examples and checks links, style and flag drift.
- `src/docs_static.rs` and `build.rs` generate completions and the man page under `OUT_DIR`; `tests/docs_examples.rs` checks that CLI output matches. No generated `dist/` belongs in the source tree.
- Attestation fixtures in `tests/fixtures/attestation/` support offline verification tests. Test-only hooks in `src/http.rs` and `src/verify/mod.rs` are gated behind `test-hooks` and ignored by normal release binaries.
