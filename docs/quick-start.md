# Quick start

## Install

Install `tuishou` with cargo or from a release binary. Release binaries are verified
in process against their GitHub attestations; no GitHub CLI is required.

## Minimal config

Create `tuishou.toml`:

```toml
[install]
method = "copy"
source = "github"
repo = "owner/name"
asset = "tool-{version}-{target}.tar.gz"

[verify]
signer_workflow = ".github/workflows/release.yml"

[service]
name = "tool.service"
mode = "user"
```

## Verify and install

```sh
tuishou verify tool-1.2.3-x86_64-unknown-linux-gnu.tar.gz \
  --repo owner/name --tag v1.2.3
tuishou install
tuishou status
```

`tuishou verify` prints the digest, issuer, identity, workflow, ref, and Rekor log
index; exit 0 means verified, 1 refused, 2 usage. `install` refuses unverified
artifacts unless you explicitly pass `--allow-unattested`.

## Enable auto-update

```toml
[auto_update]
enabled = true
check_interval = "6h"
```

Auto-update never accepts `--allow-unattested`. It checks releases, verifies in
process, and applies only verified artifacts. See [auto-update.md](auto-update.md) and
[verify-release.md](verify-release.md).
