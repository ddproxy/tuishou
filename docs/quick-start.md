# Quick start

## Install

Install `tuishou` with cargo or from a release binary. Release binaries are verified
in process against their GitHub attestations; no GitHub CLI is required.

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
tuishou verify reiki-v0.2.1-x86_64-unknown-linux-musl.tar.gz \
  --repo ddproxy/reiki --tag v0.2.1
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
