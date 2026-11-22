# Tuishou

[![CI](https://github.com/ddproxy/tuishou/actions/workflows/ci.yml/badge.svg)](https://github.com/ddproxy/tuishou/actions/workflows/ci.yml)
[![CodeQL](https://github.com/ddproxy/tuishou/actions/workflows/codeql.yml/badge.svg)](https://github.com/ddproxy/tuishou/actions/workflows/codeql.yml)
[![OpenSSF Scorecard](https://api.securityscorecards.dev/projects/github.com/ddproxy/tuishou/badge)](https://securityscorecards.dev/viewer/?uri=github.com/ddproxy/tuishou)
[![Release](https://img.shields.io/github/v/release/ddproxy/tuishou)](https://github.com/ddproxy/tuishou/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Install, update and roll the Tuishou stack from GitHub releases. `tuishou` manages release artifacts for components such as [Lu](https://github.com/ddproxy/lu), verifies them before install, and rolls them across a fleet over SSH.

This repository holds the installer, updater and fleet roll tooling only. Controller and agent code live in the Lu repository.

## Install

Download the release archive for your target, verify its checksum, and place `tuishou` on your `PATH`:

```sh
TAG=v0.1.0
TARGET=x86_64-unknown-linux-musl
curl -fsSLO "https://github.com/ddproxy/tuishou/releases/download/${TAG}/tuishou-${TAG}-${TARGET}.tar.gz"
curl -fsSLO "https://github.com/ddproxy/tuishou/releases/download/${TAG}/tuishou-${TAG}-${TARGET}.tar.gz.sha256"
sha256sum -c "tuishou-${TAG}-${TARGET}.tar.gz.sha256"
tar -xzf "tuishou-${TAG}-${TARGET}.tar.gz"
install -m 0755 tuishou ~/.local/bin/tuishou
```

## Commands

```sh
# Install a component (latest release)
tuishou install lu

# Install a specific version into a prefix
tuishou install lu --version v0.4.0 --prefix /opt/tuishou/bin

# Install every known component
tuishou install all --allow-unattested

# Check for updates (exit 10 when updates exist)
tuishou update --check

# Update installed components
tuishou update --allow-unattested

# Show installed and latest versions
tuishou status --json

# Roll back a component to the previous binary
tuishou rollback lu

# Update the tuishou binary itself from its own release
tuishou self-update --version v0.2.0

# Print shell completions or the man page
tuishou completions bash
tuishou man

# Roll a component across a fleet
tuishou roll --nodes inventory.toml --component lu --version v0.4.0 --dry-run
```

Global flags: `--json`, `--quiet`, `--verbose`. `NO_COLOR` is respected.

## Documentation

- [Quick start](docs/quick-start.md)
- [Install and update](docs/install-update.md)
- [Verify a tuishou release](docs/verify-release.md)
- [Fleet roll](docs/fleet-roll.md)
- [Security](docs/security.md)
- [FAQ](docs/faq.md)

## Development

```sh
make build
make test
make lint
make docs
make release-check
```

## License

[MIT](LICENSE)
