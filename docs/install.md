# Install

For operators installing or upgrading tuishou from a release.

## One line

```sh
curl -fsSL https://raw.githubusercontent.com/ddproxy/tuishou/main/install.sh | bash -s -- --components reiki,lu
```

Download, read, then run instead:

```sh
curl -fsSLO https://raw.githubusercontent.com/ddproxy/tuishou/main/install.sh
less install.sh
bash install.sh --help
bash install.sh --components reiki,lu
```

## Options

| Option | Default | Meaning |
| --- | --- | --- |
| `--components reiki,lu` | `reiki,lu` | Comma-separated components; `tuishou` is always installed. Known names: `reiki`, `lu`. |
| `--version <tag>` | latest stable | tuishou release tag; normalised with a leading `v`. |
| `--prefix <dir>` | `/usr/local` as root, else `~/.local` | Install root; binaries go in `<dir>/bin`. |
| `--no-components` | off | Install `tuishou` only. |
| `--dry-run` | off | Resolve the release and print the plan; make no changes. |
| `--yes` | off | Replace an existing binary without a prompt. |
| `--help` | - | Print usage and exit 0. |

## What the installer does

1. Detects the platform. Only Linux x86_64 (`x86_64-unknown-linux-musl`) is installed by this script; other platforms exit 3 and list the release targets that exist.
2. Fetches release metadata from `https://api.github.com/repos/ddproxy/tuishou/releases/latest` (or the tagged endpoint), and downloads the archive and `.sha256` asset.
3. Verifies SHA-256 in the shell, extracts `tuishou` into a private temporary directory inside the prefix, and stages it into `<prefix>/bin` with a rename. The previous binary is copied to `tuishou.prev`.
4. Runs `<prefix>/bin/tuishou verify <archive> --repo ddproxy/tuishou --tag <tag>`. On failure it removes the new binary, restores the previous one if present, and exits 4.
5. Records the binary with `tuishou adopt`, then runs `tuishou install <component> --prefix <prefix>/bin` for each requested component. Component installs verify attestations in process.

`TUISHOU_GITHUB_API_BASE` redirects release metadata and asset URLs to a test server. Release binaries also use it for attestation API calls; test-only loopback hooks are ignored by normal builds.

## Manual install

```sh
TAG=<tag>
TARGET=x86_64-unknown-linux-musl
curl -fsSLO "https://github.com/ddproxy/tuishou/releases/download/${TAG}/tuishou-${TAG}-${TARGET}.tar.gz"
curl -fsSLO "https://github.com/ddproxy/tuishou/releases/download/${TAG}/tuishou-${TAG}-${TARGET}.tar.gz.sha256"
sha256sum -c "tuishou-${TAG}-${TARGET}.tar.gz.sha256"
tar -xzf "tuishou-${TAG}-${TARGET}.tar.gz" tuishou
./tuishou verify "tuishou-${TAG}-${TARGET}.tar.gz" --repo ddproxy/tuishou --tag "$TAG"
install -m 0755 tuishou "$HOME/.local/bin/tuishou"
```

The manual path checks the checksum before extracting and checks the attestation before installation. It does not create `tuishou.prev`; add `tuishou adopt tuishou --version "$TAG" --prefix "$HOME/.local/bin"` to record it in state.

## Exit codes

`0` ok, `2` usage, `3` unsupported platform, `4` verification failed, `5` install failed. The binary itself uses `0` ok, `1` error, `2` usage and `10` updates available for `update --check`.
