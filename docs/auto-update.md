# Auto-update

`tuishou` can check GitHub releases, verify the matching artifacts in process, download
them, and apply them without a system service.

## Enable it

```toml
[auto_update]
enabled = true
include_prerelease = false
check_interval = "6h"

[verify]
signer_workflow = ".github/workflows/release.yml"
cache_ttl = "24h"
```

`auto_update.check_interval` is a Go-style duration. Verification settings are shared
with installs and updates; see [verify-release.md](verify-release.md).

## What happens

1. GitHub's release API is checked for the newest non-prerelease (or prerelease, if
   enabled). Network access uses the HTTP allowlist, timeouts, size caps, and redirect
   limits; `GITHUB_TOKEN`/`GH_TOKEN` is used only for `api.github.com`.
2. Each release asset is verified against its GitHub attestation in process:
   artifact digest, Sigstore/Fulcio chain, Rekor SET and inclusion proof, signing-time
   validity, and the GitHub OIDC workflow identity and SLSA provenance. A TUF refresh
   may contact `tuf-repo-cdn.sigstore.dev`.
3. Only verified assets are downloaded, checked against expected digests, and applied.
4. A forced check marks the state and writes a receipt.

If verification fails or no attestation is available, auto-update refuses to apply the
release. `--allow-unattested` is rejected in `auto` mode: auto-update never downgrades
to unverified installs.

## State and receipts

By default, state is under `~/.local/state/tuishou` (or `$XDG_STATE_HOME/tuishou`):

```text
state/last-check.json
state/known.json
state/log.jsonl
state/update.json
state/receipts/
```

The verifier cache defaults to a `0700` directory under the state directory unless
`[verify].cache_dir` is set.

## Scope

Auto-update covers binaries `tuishou` manages via `install.source = "github"`. Remote
roll uses `ssh` and runs `tuishou install` on the target; forward the same verification
configuration and credentials there. See [dogfooding.md](dogfooding.md) for an example.
