# Quick start

## Install

Install `tuishou` with cargo or from a release binary. Release binaries are verified
in process against their GitHub attestations; no GitHub CLI is required.

## Bootstrap trust

Before `tuishou` exists, the installer can check digests but cannot verify an
attestation. The `.sha256` asset comes from the same release as the archive, so
it is a consistency check, not a trust bootstrap. The `tuishou verify` step runs
the binary that was just installed, so in-process attestation cannot verify the
verifier itself.

Fetch the archive digest from an independent trusted channel (release records,
a signed note or a configuration-management record) and pin it:

```sh
BOOTSTRAP_SHA256=$(awk '{ print $1; exit }' /etc/tuishou/bootstrap.sha256)
curl -fsSL https://raw.githubusercontent.com/ddproxy/tuishou/main/install.sh | bash -s -- --components reiki,lu --expect-sha256 "$BOOTSTRAP_SHA256"
```

`--expect-sha256` takes 64 hex characters, case-insensitive. It is checked
before extraction; a mismatch exits 4 and installs nothing. Without it the
one-line install still works, with the bootstrap limitation above. Mandatory
pinned digests and signed packages are a residual design decision, not
implemented in this build. See [install.md](install.md) and
[security.md](security.md).

## Defaults

`tuishou` manages `reiki`, `lu` and `tuishou` out of the box. See
[components.md](components.md) for repositories, assets, binaries, service units and
health checks.

## Config

Configuration is optional. Create `tuishou.toml` only when you want to change policy:

```toml
[verify]
signer_workflow = ".github/workflows/release.yml"

[auto_update]
enabled = true
interval = "10m"
components = ["reiki", "lu"]
```

The config file must be owned by you or root and must not be writable by group or
others. Unknown keys are errors. Component overrides may change paths and policy but
never the repository or asset host.

## Verify and install

```sh
tuishou verify reiki-v0.3.2-x86_64-unknown-linux-musl.tar.gz \
  --repo ddproxy/reiki --tag v0.3.2
tuishou install reiki
tuishou status
```

`tuishou verify` prints the digest, issuer, identity, workflow, ref and Rekor log
index; exit 0 means verified, 1 refused, 2 usage. `install` refuses unverified
artifacts unless you explicitly pass `--allow-unattested`.

`install all` attempts every default component and reports each one. A component whose
binaries are absent from the latest release is `not in this release`, not a failure.

## Check and update

```sh
tuishou update --check
tuishou update
tuishou update reiki
```

A bare `tuishou update` means `update all`. `update --check` exits 0 when nothing
installable is pending, 10 when an installed component has an installable update, 1 on
an evaluation error, and 2 for usage errors. Every component gets exactly one status
line with the fixed vocabulary listed in [components.md](components.md).

## Auto-update

Auto-update is off by default. Enable it only if you want releases installed without
human review:

```toml
[auto_update]
enabled = true
interval = "10m"
components = ["reiki", "lu"]
```

Auto-update never accepts `--allow-unattested`. See [auto-update.md](auto-update.md)
and [verify-release.md](verify-release.md).
