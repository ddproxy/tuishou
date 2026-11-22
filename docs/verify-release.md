# Verify a tuishou release

Every `tuishou` release publishes two files for each target:

- `tuishou-<tag>-<target>.tar.gz` — the release archive.
- `tuishou-<tag>-<target>.tar.gz.sha256` — its SHA-256 checksum.

`<tag>` looks like `v0.1.0` and `<target>` looks like
`x86_64-unknown-linux-musl`.

## Verify the checksum

Download both files from the same release and check the archive against the
checksum file:

```sh
TAG=v0.1.0
TARGET=x86_64-unknown-linux-musl
BASE="https://github.com/ddproxy/tuishou/releases/download/${TAG}"

curl -fsSLO "${BASE}/tuishou-${TAG}-${TARGET}.tar.gz"
curl -fsSLO "${BASE}/tuishou-${TAG}-${TARGET}.tar.gz.sha256"
sha256sum -c "tuishou-${TAG}-${TARGET}.tar.gz.sha256"
```

`sha256sum -c` must print `OK`. If it does not, discard both files and try the
download again. Do not extract an archive whose checksum does not match.

## Verify the build attestation

Release archives are attested with GitHub build provenance. If the `gh` CLI is
installed and authenticated, verify the archive before extracting it:

```sh
gh attestation verify "tuishou-${TAG}-${TARGET}.tar.gz" --repo ddproxy/tuishou
```

`tuishou install` and `tuishou self-update` run this check automatically when
`gh` is on `PATH`. Without `gh`, they fail closed unless you pass
`--allow-unattested`; in that case only the checksum is verified.

## Install or update after verification

Prefer letting `tuishou` repeat the verification itself:

```sh
# Install the release archive as a component (same checks as other components)
tuishou install tuishou --version "${TAG}" --allow-unattested

# Or update the running binary from its own release archive
tuishou self-update --version "${TAG}" --allow-unattested
```

Both commands download the archive and `.sha256`, verify SHA-256, verify the
build attestation when `gh` is available, extract only the `tuishou` binary,
and atomically replace it. The previous binary is kept as `tuishou.prev`.

## Rehearse locally

Maintainers can reproduce the release layout without publishing anything:

```sh
make release-check
```

The target builds `dist/tuishou-<tag>-<target>.tar.gz` plus `.sha256`, serves
them from a local fake release, runs `tuishou install` against that server, and
checks that the installed binary reports the expected `--version`.
