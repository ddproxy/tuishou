# Changelog

## Unreleased

### Added

- Opt-in scheduled auto-update (`tuishou auto-update ...`): off by default, documented as not recommended for production. One-shot cycles under an exclusive flock, systemd timer printer (no daemon), hold/release/clear-bad controls, ETag/rate-limit aware release listing, `min_release_age`, patch/minor/any policy with 0.y minor treated as breaking, mandatory attestation (`gh attestation verify --signer-workflow`, `--allow-unattested` refused), node agent drain/install/restart/health/undrain with rollback and bad-version marking, conservative controller policy (manual default, when_idle/window with backup retention and schema-change marker refusal), single-line journal logging, and optional notify argument vector.
- `tuishou adopt <component> --version <tag>` records an existing binary in state without replacing it (dogfooding path for hand-installed nodes).
- Default component table points at `ddproxy/tuishou-lu` with real release naming `lu-<tag>-<target>.tar.gz`; `lu-agent` accepted while the rename to `ting` lands.
- `docs/auto-update.md`, `docs/dogfooding.md`, and an Ansible-friendly status/adopt snippet. `tuishou status --json` documented as a stable automation schema.

### Changed

- Hardened `scripts/validate-workflows.sh`: remote actions must use `owner/repo[/path]@<40 lowercase hex>` with an optional version comment; short/long/uppercase SHAs and branch refs are rejected. Digest-pinned `docker://` images and local `./` actions remain allowed. Added positive and negative fixtures plus fixture tests (`scripts/test-validate-workflows.sh`, `tests/validator.rs`).
- Added `tuishou self-update`: updates the running binary from its own release archive using the same checksum and attestation verification, atomic replace, and `.prev` retention as component installs.
- Added `make release-check` and `scripts/release-check.sh`: builds the local release archive layout for the host target, serves it from a fake release, installs with `tuishou install`, and verifies the installed binary's `--version`.
- Added shell completions (`tuishou completions bash|zsh|fish`), a man page (`tuishou man`), and build-time generation of both into gitignored `dist/`.
- Added a "Verify a tuishou release" guide and a test that parses and dry-runs the documented `roll` inventory example.

## v0.1.0 - 2026-11-21

- Initial release of `tuishou`.
- Install, update, status, rollback, and fleet roll commands.
- SHA-256 verification, optional `gh` attestation verification, and safe archive extraction.
- Atomic installs with `.prev` retention and JSON state.
