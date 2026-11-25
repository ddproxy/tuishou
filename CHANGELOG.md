# Changelog

## Unreleased

### Added

- `[auto_update] self = true` replaces the running `tuishou` binary at the end of an auto-update cycle.

## [0.3.6]

### Added

- `[auto_update] channel = "dev"` accepts dev pre-releases for agent and Reiki nodes.

### Changed

- Workflow identity errors name both refs and `--signer-workflow`.

## [0.3.5]

### Fixed

- Reiki health URLs on node-owned IPs accept cleartext http.

## [0.3.4]

### Fixed

- Agent health reads the heartbeat age reported by `ting status --json`.

## [0.3.3]

### Changed

- Breaking: Agent updates use `ting` drain commands and require a Lu release with self-drain support.
- Node names resolve from `[auto_update].node_name`, `TUISHOU_NODE_NAME`, the ting agent name, then hostname.

### Fixed

- Heartbeat freshness accepts Lu's numeric `last_seen` timestamp.

## [0.3.2]

### Fixed

- Attestation verification considers only SLSA build provenance.
- Source commits resolve from the release tag, including annotated tags.
- Hosts without Reiki config or unit get a plain binary swap.

## [0.3.1]

### Fixed

- A host counts as a Lu agent node only when its agent unit is installed and enabled.

## [0.3.0]

### Added

- `install.sh` installs `tuishou` and components with checksum and attestation checks, staging and rollback.
- `tuishou top` hosts Reiki and Lu TUIs in tabs.
- `[update].start_delay_max` and `[update].max_unavailable` control update delays and controller availability.

### Changed

- Breaking: Reiki and Lu are default components; `ting` is not a default.
- Breaking: A bare `tuishou update` updates all components; adding a missing component requires a named `install` or `update`.
- Breaking: `tuishou verify` requires `--tag`.
- Breaking: Config files require owner or root ownership and no group or other write access.
- Breaking: Generated units pin `PATH` and restrict `TUISHOU_TIMER_EXE` to approved paths.
- Commands print one status line per component; `--json` reports the same data.
- Reiki updates drain a Lu node, check config, restart, health-check and roll back on failure.
- `--allow-unattested` skips staged and smoke checks when attestation fails.

### Fixed

- Rollback checks recorded checksums and skips optional binaries without a previous copy.
- Reiki recovery keeps a node drained when restore, restart or health checks fail.

### Removed

- Breaking: `tuishou roll`, the remote SSH runner and the inventory format are removed; their config keys are unknown.

## v0.2.1 - 2026-11-22

### Changed

- Breaking: Release archives target x86_64 Linux only.

## v0.2.0 - 2026-11-22

### Added

- `tuishou auto-update` runs opt-in one-shot cycles with hold, status and bad-version controls.
- `tuishou adopt <component> --version <tag>` records an installed binary in state.
- `tuishou self-update` replaces the running binary from its own release archive.
- `tuishou completions` and `tuishou man` generate shell completions and a man page.

### Changed

- `tuishou verify <file>` verifies releases without an external CLI and prints attestation details.
- `[verify]` sets the signer workflow, trust roots, cache and cache lifetime.
- `--config <path>` and `TUISHOU_CONFIG` select the config file.
- Release selection uses full version precedence and honors `min_release_age`.
- `auto-update` actions and `adopt` accept `--prefix` and honor `auto_update.prefix`.
- `auto-update install-timer` uses the configured interval with a 5m minimum.
- Controller updates refuse releases with missing notes or an intermediate schema-change marker.
- Failed health or recovery checks keep a node drained and restore the previous version.

### Security

- Tokens go only to GitHub API requests and are redacted from errors and logs.

## v0.1.0 - 2026-11-21

### Added

- Initial release provides install, update, status, rollback, SHA-256 checks and atomic installs.
