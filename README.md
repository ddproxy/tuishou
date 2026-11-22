# Tuishou

[![CI](https://github.com/ddproxy/tuishou/actions/workflows/ci.yml/badge.svg)](https://github.com/ddproxy/tuishou/actions/workflows/ci.yml)
[![CodeQL](https://github.com/ddproxy/tuishou/actions/workflows/codeql.yml/badge.svg)](https://github.com/ddproxy/tuishou/actions/workflows/codeql.yml)
[![OpenSSF Scorecard](https://api.securityscorecards.dev/projects/github.com/ddproxy/tuishou/badge)](https://securityscorecards.dev/viewer/?uri=github.com/ddproxy/tuishou)
[![Release](https://img.shields.io/github/v/release/ddproxy/tuishou)](https://github.com/ddproxy/tuishou/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Install and update the Tuishou stack from GitHub releases. `tuishou` manages release artifacts for [Reiki](https://github.com/ddproxy/reiki) and [Lu](https://github.com/ddproxy/tuishou-lu), verifies them before install, and updates each node locally.

This repository holds the installer and updater only. The component code lives in each component's repository; see [components.md](docs/components.md).

## Install

Download the release archive for your target and place `tuishou` on your `PATH`:

```sh
TAG=v0.2.1
TARGET=x86_64-unknown-linux-musl
curl -fsSLO "https://github.com/ddproxy/tuishou/releases/download/${TAG}/tuishou-${TAG}-${TARGET}.tar.gz"
tar -xzf "tuishou-${TAG}-${TARGET}.tar.gz"
install -m 0755 tuishou ~/.local/bin/tuishou
```

Release archives have GitHub attestations. `tuishou` verifies them in process; no GitHub CLI is required. See [verify a release](docs/verify-release.md).

## Commands

```sh
# Install a component (latest release, verified in process)
tuishou install reiki
tuishou install lu

# Install a specific version into a prefix
tuishou install lu --version v0.4.0 --prefix /opt/tuishou/bin

# Record an existing binary without replacing it
tuishou adopt lu --version v0.4.0 --prefix /usr/local/bin

# Check every default component (0 none, 10 update available, 1 error, 2 usage)
tuishou update --check

# Update every default component (same as `update all`)
tuishou update

# Inspect an artifact's attestation
tuishou verify reiki-v0.2.1-x86_64-unknown-linux-musl.tar.gz \
  --repo ddproxy/reiki --tag v0.2.1

# Show one fixed-vocabulary status line per component (stable JSON with --json)
tuishou status
tuishou status --json

# Roll back a component to the previous binary
tuishou rollback reiki

# Opt-in scheduled auto-update (off by default, not recommended for production)
tuishou auto-update status --json
tuishou auto-update install-timer --interval 10m

# Update the tuishou binary itself from its own release
tuishou self-update --version v0.2.0

# Print shell completions or the man page
tuishou completions bash
tuishou man
```

Global flags: `--json`, `--quiet`, `--verbose`, `--config <path>` (or `TUISHOU_CONFIG`). `NO_COLOR` is respected.

`--allow-unattested` is an explicit local opt-in that warns and continues when verification fails. Auto-update refuses it.

## Documentation

- [Components](docs/components.md)
- [Quick start](docs/quick-start.md)
- [Install and update](docs/install-update.md)
- [Node update procedure](docs/node-update.md)
- [Verify a tuishou release](docs/verify-release.md)
- [Scheduled auto-update (opt-in)](docs/auto-update.md)
- [Dogfooding / adoption](docs/dogfooding.md)
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
