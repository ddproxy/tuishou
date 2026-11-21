# Security

## Verified

- **SHA-256 checksums**: every downloaded archive is verified against its `.sha256` file before extraction.
- **Build attestations**: when `gh` is installed, `tuishou` runs `gh attestation verify <archive> --repo <owner/repo>`. If `gh` is absent, verification continues only with `--allow-unattested`.
- **Archive safety**: extraction rejects absolute paths, `..` traversal, symlinks, hardlinks, and device entries. Only expected binary names are extracted.
- **HTTPS-only downloads**: release and asset URLs must use HTTPS. GitHub tokens from `GITHUB_TOKEN` or `GH_TOKEN` are used only when set and are never logged.
- **Atomic install**: binaries are written to a temporary file in the target directory, mode `0755`, then renamed into place. The previous binary is kept as `<name>.prev`.

## Not verified

- **Runtime behavior** of installed binaries after install.
- **Remote host configuration** during fleet rolls beyond the commands `tuishou` runs.
- **macOS targets** are listed but unverified.

## Failure behavior

Checksum, attestation, or archive mismatch causes a fail-closed error. A failed install leaves the previous binary in place.
