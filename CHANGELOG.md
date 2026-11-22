# Changelog

## Unreleased

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
