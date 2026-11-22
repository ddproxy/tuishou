# Install and update

## Install

```sh
tuishou install <component|all> [--version vX.Y.Z] [--prefix DIR] [--dry-run] [--allow-unattested] [--allow-major]
```

- Resolves the release from GitHub (latest by default).
- Downloads the archive and `.sha256` checksum into a temporary directory.
- Verifies SHA-256.
- Verifies build attestation with `gh attestation verify` when `gh` is installed. If `gh` is absent, pass `--allow-unattested` to continue with checksum verification only.
- Extracts only expected binary names from the archive. Absolute paths, `..`, and symlinks are rejected.
- Installs atomically: write to a temp file in the target directory, `chmod 0755`, rename over.
- Keeps the previous binary as `<name>.prev`.
- Records the install in a state file: `$XDG_STATE_HOME/tuishou/installed.json`, or `<prefix>/.tuishou.json` when `--prefix` is set.

## Update

```sh
tuishou update [component|all] [--check] [--allow-major] [--allow-unattested] [--prefix DIR]
```

- Compares installed versions with the latest release.
- `--check` reports updates and exits with code 10 when updates exist.
- Updates do not cross a major version boundary unless `--allow-major` is passed.
- When release notes contain a heading for the new version, `tuishou` prints it.

## Status

```sh
tuishou status [--json] [--prefix DIR]
```

Shows installed versions, latest versions, and whether a component service unit is active when `systemctl` is present.

## Rollback

```sh
tuishou rollback <component> [--prefix DIR]
```

Restores `<name>.prev` for the component binaries.

## Self-update

```sh
tuishou self-update [--version vX.Y.Z] [--prefix DIR] [--allow-unattested] [--allow-major] [--dry-run]
```

Updates the `tuishou` binary itself from its own release archive. It uses the
same checksum and attestation verification as component installs, writes the
new binary atomically next to the running executable (or into `--prefix`), and
keeps the old binary as `tuishou.prev`. See
[Verify a tuishou release](verify-release.md) for manual verification steps.

## Generated docs

`cargo build` writes shell completions (`dist/tuishou.bash`, `dist/tuishou.zsh`,
`dist/tuishou.fish`) and the man page (`dist/tuishou.1`) into `dist/`. Those
files are generated at build time and are not committed. The same content is
available on demand via `tuishou completions <bash|zsh|fish>` and `tuishou man`.
