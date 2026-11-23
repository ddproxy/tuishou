# Changelog

## Unreleased

### Removed

- Removed `tuishou roll`, the remote SSH runner and the inventory file format, together with their docs, examples, man page entries, completions and tests. Config sections belonging to them are now unknown keys. `tuishou update` and `auto-update` are the only update paths; each node updates itself.

### Added

- Added `[update].start_delay_max` (default `0`, recommended `10m` for fleets) for a random delay before an update cycle, and `[update].max_unavailable` (default unset) to skip a cycle when the controller already reports enough draining or offline nodes. Both are honoured by `update` and `auto-update`; unreadable controller state fails closed when a limit is set.
- Added `docs/node-update.md` with the single-node drain/install/restart/health/rollback/undrain procedure, manual invocation, scheduling guidance and an "Updating many nodes" section with an Ansible example. Added tests for the removed command and config keys, jitter bounds, fleet-limit allow/refuse/fail-closed behaviour and the shared local update path.
- `docs/components.md` documents the built-in Reiki, Lu and Tuishou defaults, their release assets, binaries, service units, signer workflows and health checks. Man page, completions and examples now use Reiki and Lu first.
- New integration coverage for bare `update`, unknown component usage errors, shared `--check` exit codes, per-component reporting, `install all` with a missing binary, config trust, and rejected repository overrides. Auto-update tests cover Reiki-before-Lu ordering, Reiki drain/config-check/restart/health and rollback with a bad-version mark.
- Added `install.sh`, a single-script installer for Linux x86_64 (`x86_64-unknown-linux-musl`) with `--components`, `--version`, `--prefix`, `--no-components`, `--dry-run`, `--yes` and `--help`. It fetches release metadata, checks SHA-256, stages the binary, runs `tuishou verify` on its own archive, records it with `tuishou adopt`, installs the requested components, and prints an install summary with next commands. It never invokes `sudo`; an unwritable prefix prints a root rerun command. `NO_COLOR` and non-TTY output suppress color. Exit codes are `0` ok, `2` usage, `3` unsupported platform, `4` verification failed and `5` install failed.
- Added the bootstrap verification sequence: the shell checks the `.sha256` asset before a verifier exists, then runs the downloaded binary's `verify` command; failure removes the new binary and exits 4. Installs are staged and keep one `tuishou.prev` rollback copy.
- Added a local fake release server (`tests/fixtures/installer/`) and 11 installer cases in `tests/installer_test.sh`: usage errors, unsupported platforms, dry run, checksum mismatch, attestation failure, unwritable prefixes, component installs, quickstart proof, idempotency, rollback restore and `--no-components`. `make installer-test` runs them; `make test` and CI run the same script.
- Docs rewrite: README template and docs index, plus quickstart, install, configuration, operations, security, limits, troubleshooting, CLI and architecture pages, `llms.txt` and `AGENTS.md`. `scripts/docs-check.sh` runs quickstart commands, validates config examples and checks links. `tuishou top` is described as planned only; it is not in this build.

### Changed

- Reiki and Lu are equal first-class defaults alongside Tuishou. `ting` is no longer a default; the recognition path remains for a future one-line return.
- Every command now prints exactly one fixed-vocabulary status line per component in table order, and `--json` carries the same data. `status`, `update --check` and `auto-update` share one evaluation.
- A bare `tuishou update` means `update all`. Components absent from a release are reported as `not in this release` and never counted as updates. Not-installed components are installed only by an explicit `install <name>` or `update <name>`.
- Config files are refused unless owned by the running user or root and not group/other writable. Component overrides may change paths and policy but never the repository or asset host.
- Reiki updates drain a Lu node when present, check the Reiki config before restart, health-check after restart and roll back with a bad-version mark on failure.
- Installer failures are fail closed: checksum and attestation refusals leave no unverified binary at the destination, and a previous binary is restored from `tuishou.prev` when present. Unsupported platforms exit 3 and list the existing release targets; component installs fail closed unless `--allow-unattested` is passed explicitly (auto-update still refuses it).

### Fixed

- Rollback skips optional binaries that have no previous copy, so Lu controller-only archives restore cleanly.
- A failed bootstrap attestation no longer leaves an unverified `tuishou` in the bin directory; the new binary is removed and the previous one restored when present.
- An unwritable prefix now exits 5 with the exact root rerun command instead of failing later in the install. `--dry-run` resolves and reports a release without touching the prefix.

## v0.2.1 - 2026-11-22

### Changed

- Release archives are built for x86_64 Linux only. The aarch64 build failed to link with the new verification dependencies; it returns once the cross toolchain is fixed.

## v0.2.0 - 2026-11-22

### Changed

- Attestation verification now runs in process via the official `sigstore` Rust crate instead of spawning an external CLI. No external GitHub CLI, login, or subprocess is required for install, update, self-update, roll, or auto-update. Verification is fail closed across all paths; auto-update still refuses `--allow-unattested`.
- Added a `[verify]` config section with `signer_workflow`, per-component overrides, embedded/pinned or operator-managed trust roots, a `0700` cache directory, and a configurable `cache_ttl`. Tokens are read only from `GITHUB_TOKEN`/`GH_TOKEN`, sent only to `api.github.com`, and stripped on redirects.
- Added `tuishou verify <file> --repo owner/name [--signer-workflow path] [--tag tag]`, which prints the digest, issuer, identity, workflow, ref, and Rekor log index. Exit status is 0 verified, 1 refused, 2 usage.
- Replaced external-CLI test scaffolding with fake verifiers and real signed-bundle fixtures; CI now proves tests pass with no external CLI on PATH and with a failing binary first on PATH.

### Fixed

- Auto-update cycles now hold a kernel-managed advisory lock for the whole cycle; a killed process cannot leave a stale `lock.d` behind, and hold/create/release/expiry operations share the same lock.
- State and hold writes use unique temporary files, `sync_all`, rename and directory fsync; concurrent writers can no longer collide on `auto.json.tmp`.
- Every `lu`/`systemctl`/`ssh`/notifier child uses an argument vector and a hard deadline; timed-out children are killed and reaped. Command execution is behind a thin runner so failure paths are unit-testable.
- Drain and undrain now call the real Lu 0.4 commands (`lu drain <node>`, `lu undrain <node>`). Undrain failures are propagated; drain state comes only from `lu nodes --json`, requires an explicit `draining: true` with no current job, and uses exact version comparison (`0.4.10` no longer matches `0.4.1`).
- Agent and controller installs are transactional: staged binaries are smoke-checked before commit, the whole set commits atomically, the last good `.prev` is preserved, and state is recorded only after health confirmation.
- Health/recovery failures keep a node drained when verification fails, restore and verify the previous version, persist the bad-version mark immediately, and report the real command error text.
- Controller idleness now requires all four `lu status --json` job counters (`leased`, `running`, `recovering`, `needs_input`) to be present non-negative integers and zero; any other shape fails closed. Backups run as `lu backup`.
- Controller and agent binary ownership no longer overlap: `lu` owns only `lu`, while `ting` owns `ting` and `lu-agent`. Controller updates restart and health-check `controller_unit`.
- Controller schema gating checks every release up to the candidate, fails closed when release notes are missing, and refuses the `Requires: new controller database` marker in any intermediate release.
- GitHub tokens are attached only to GitHub API requests, never to asset or allowed hosts, and are stripped from headers on any cross-origin redirect. Token values are redacted from HTTP errors, journal fields, notifications, child-process output and top-level CLI errors.
- Added global `--config <path>` / `TUISHOU_CONFIG`; generated systemd units use an absolute config path and both `OnBootSec` and `OnActiveSec`.
- Controller backups now call `lu backup --config <path> --out <file>` with the loaded config, write to a unique mode `0600` file in a mode `0700` directory, fail closed on missing or empty output, and prune to `backup_keep`.
- Release selection uses full version precedence, including pre-releases, so `v1.2.1-rc.10` ranks above `v1.2.1-rc2`. Releases without a usable `published_at` are skipped when `min_release_age` is configured.
- Agent auto-update checks the verified archive contents before draining; a release with none of the expected agent binaries is recorded as `skipped` instead of failing the cycle.
- Every `auto-update` action and `adopt` accept `--prefix` and honour `auto_update.prefix`; hold, status, bad-version state, lock files and generated service units all use the resolved absolute prefix.
- `auto-update install-timer` now defaults to the validated configured interval, still enforces the 5m minimum, and includes `--prefix <absolute path>` in `ExecStart` when a prefix is in effect.
- CI/test guidance and fixtures use the real Lu interface with an empty `HOME` and a non-root user; docs and generated completions/man page were updated accordingly.

### Added

- Opt-in scheduled auto-update (`tuishou auto-update ...`): off by default, documented as not recommended for production. One-shot cycles under an exclusive flock, systemd timer printer (no daemon), hold/release/clear-bad controls, ETag/rate-limit aware release listing, `min_release_age`, patch/minor/any policy with 0.y minor treated as breaking, mandatory in-process attestation verification with the configured signer workflow (`--allow-unattested` refused), node agent drain/install/restart/health/undrain with rollback and bad-version marking, conservative controller policy (manual default, when_idle/window with backup retention and schema-change marker refusal), single-line journal logging, and optional notify argument vector.
- `tuishou adopt <component> --version <tag>` records an existing binary in state without replacing it (dogfooding path for hand-installed nodes).
- Default component table points at `ddproxy/tuishou-lu` with real release naming `lu-<tag>-<target>.tar.gz`; `lu-agent` accepted while the rename to `ting` lands. Component configs may set per-release `binary_ranges` for expected archive contents.
- `docs/auto-update.md`, `docs/dogfooding.md`, and an Ansible-friendly status/adopt snippet. `tuishou status --json` documented as a stable automation schema.

### Changed

- Hardened `scripts/validate-workflows.sh`: remote actions must use `owner/repo[/path]@<40 lowercase hex>` with an optional version comment; short/long/uppercase SHAs and branch refs are rejected. Digest-pinned `docker://` images and local `./` actions remain allowed. Added positive and negative fixtures plus fixture tests (`scripts/test-validate-workflows.sh`, `tests/validator.rs`).
- Added `tuishou self-update`: updates the running binary from its own release archive using the same checksum and in-process attestation verification, atomic replace, and `.prev` retention as component installs.
- Added `make release-check` and `scripts/release-check.sh`: builds the local release archive layout for the host target, serves it from a fake release, installs with `tuishou install`, and verifies the installed binary's `--version`.
- Added shell completions (`tuishou completions bash|zsh|fish`), a man page (`tuishou man`), and build-time generation of both into gitignored `dist/`.
- Added a "Verify a tuishou release" guide and a test that parses and dry-runs the documented `roll` inventory example.

## v0.1.0 - 2026-11-21

- Initial release of `tuishou`.
- Install, update, status, rollback, and fleet roll commands.
- SHA-256 verification and safe archive extraction.
- Atomic installs with `.prev` retention and JSON state.
