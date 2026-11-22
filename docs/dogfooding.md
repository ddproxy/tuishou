# Dogfooding

Host the release pipeline in the same repository as the deployed config: the checked-in
config, `tuishou.toml`, is the source of truth for both verification and rollout.

## Minimal config

```toml
[install]
method = "copy"
source = "github"
repo = "acme/infra"
asset = "lu-agent-{version}-{target}.tar.gz"

[verify]
signer_workflow = ".github/workflows/release.yml"
cache_ttl = "24h"

[service]
name = "lu-agent.service"
mode = "user"

[auto_update]
enabled = true
check_interval = "6h"
```

The release workflow signs artifacts with GitHub OIDC and publishes the attestation
bundle to GitHub. The verifier reads the expected signer workflow from the config and
requires the SLSA provenance predicate and GitHub identity described in
[verify-release.md](verify-release.md). No external GitHub CLI is needed on nodes; the
producer workflow publishes the release and its attestation separately.

## Check and install

```sh
tuishou verify lu-agent-1.2.3-x86_64-unknown-linux-gnu.tar.gz \
  --repo acme/infra --tag v1.2.3
tuishou install --config tuishou.toml
tuishou roll --config tuishou.toml
```

For a test install without a signed release, `--allow-unattested` warns and continues.
It is an explicit local opt-in; auto-update always refuses it.

## Remote roll

```toml
[[nodes]]
host = "deploy@10.0.0.11"

[[nodes]]
host = "deploy@10.0.0.12"
unit = "custom.service"
```

`tuishou roll` copies the binary, installs the artifact, and restarts the unit on each
node. Each node verifies in process; make sure its config and any `GITHUB_TOKEN` needed
for `api.github.com` are present. Trust-root refresh may need access to
`tuf-repo-cdn.sigstore.dev`, or pin `[verify].trust_root` to an operator-managed file.

## Safe practice

- Keep `.github/workflows/release.yml` fixed; change it only with a reviewed PR.
- Use `cache_ttl` to bound TUF refresh frequency.
- Check the reported identity, workflow, ref, and log index from `tuishou verify`.
- Upgrade one node first, then roll the rest.
