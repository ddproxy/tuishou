# Install

For operators installing or upgrading tuishou from a release.

## One line

Without a pinned digest:

```sh
curl -fsSL https://raw.githubusercontent.com/ddproxy/tuishou/main/install.sh | bash -s -- --components reiki,lu
```

Recommended: pin the archive digest from an independent trusted channel. The digest must not come from the GitHub release or its `.sha256` asset.

```sh
BOOTSTRAP_SHA256=$(awk '{ print $1; exit }' /etc/tuishou/bootstrap.sha256)
curl -fsSL https://raw.githubusercontent.com/ddproxy/tuishou/main/install.sh | bash -s -- --components reiki,lu --expect-sha256 "$BOOTSTRAP_SHA256"
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
| `--expect-sha256 <digest>` | none | 64 hex characters (case-insensitive) from an independent trusted channel; checked against the archive before extraction. Invalid format or mismatch exits 4. The release `.sha256` remains a secondary consistency check. |
| `--prefix <dir>` | `/usr/local` as root, else `~/.local` | Install root; binaries go in `<dir>/bin`. |
| `--no-components` | off | Install `tuishou` only. |
| `--dry-run` | off | Resolve the release and print the plan; make no changes. |
| `--yes` | off | Replace an existing binary without a prompt. |
| `--help` | - | Print usage and exit 0. |

## What the installer does

1. Detects the platform. Only Linux x86_64 (`x86_64-unknown-linux-musl`) is installed by this script; other platforms exit 3 and list the release targets that exist.
2. Fetches release metadata from `https://api.github.com/repos/ddproxy/tuishou/releases/latest` (or the tagged endpoint), and downloads the archive.
3. If `--expect-sha256` was supplied, compares the archive digest to the pinned value and exits 4 on mismatch. Downloads the `.sha256` asset and checks it as a secondary consistency check. Neither check extracts or runs release code.
4. Extracts `tuishou` into a private temporary directory inside the prefix, and stages it into `<prefix>/bin` with a rename. The previous binary is copied to `tuishou.prev`.
5. Runs `<prefix>/bin/tuishou verify <archive> --repo ddproxy/tuishou --tag <tag>`. On failure it removes the new binary, restores the previous one if present, and exits 4.
6. Records the binary with `tuishou adopt`, then runs `tuishou install <component> --prefix <prefix>/bin` for each requested component. Component installs verify attestations in process.

`TUISHOU_GITHUB_API_BASE` redirects release metadata and asset URLs to a test server. Release binaries also use it for attestation API calls; test-only loopback hooks are ignored by normal builds.

## Bootstrap trust

Before `tuishou` exists, the installer can check digests but cannot verify an attestation. A `.sha256` asset served with the archive proves consistency between two files from the same release; it does not prove who built the archive. The `tuishou verify` step runs the freshly installed binary, so in-process attestation cannot verify the verifier.

Fetch the archive digest out of band: release records, a signed note, or a configuration-management record kept outside the release. Pass it with `--expect-sha256` (64 hex characters, case-insensitive) to check the archive before extraction. Without it the one-line install still works, with the same residual bootstrap limitation. Mandatory pinned digests and signed packages are a residual design decision and are not implemented in this build.

## Manual install

Read the digest from your independent trusted record, build a local checksum file from it, and verify with `sha256sum -c` before extraction. Do not use the release's `.sha256` asset as the trust source.

```sh
TAG=<tag>
TARGET=x86_64-unknown-linux-musl
ARCHIVE="tuishou-${TAG}-${TARGET}.tar.gz"
BOOTSTRAP_SHA256=$(awk '{ print $1; exit }' /etc/tuishou/bootstrap.sha256)
printf '%s  %s\n' "$BOOTSTRAP_SHA256" "$ARCHIVE" > bootstrap.sha256
curl -fsSLO "https://github.com/ddproxy/tuishou/releases/download/${TAG}/${ARCHIVE}"
sha256sum -c bootstrap.sha256
tar -xzf "$ARCHIVE" tuishou
./tuishou verify "$ARCHIVE" --repo ddproxy/tuishou --tag "$TAG"
install -m 0755 tuishou "$HOME/.local/bin/tuishou"
```

`sha256sum -c` accepts upper- or lowercase digests. The manual path checks the pinned digest before extracting and checks the attestation before installation. It does not create `tuishou.prev`; add `tuishou adopt tuishou --version "$TAG" --prefix "$HOME/.local/bin"` to record it in state.

## Exit codes

`0` ok, `2` usage, `3` unsupported platform, `4` verification failed, `5` install failed. The binary itself uses `0` ok, `1` error, `2` usage and `10` updates available for `update --check`.
