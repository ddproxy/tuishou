# Dogfooding

Host the release pipeline in the same repository as the deployed config: the checked-in
config, `tuishou.toml`, is the source of truth for verification and rollout policy.
Repositories and asset hosts are built in; see [components.md](components.md).

## Config

```toml
[verify]
signer_workflow = ".github/workflows/release.yml"
cache_ttl = "24h"

[auto_update]
enabled = true
interval = "10m"
components = ["reiki", "lu"]
policy = "patch"
min_release_age = "10m"
controller_policy = "manual"
node_name = "node-1"
reiki_config_path = "/var/lib/reiki/reiki.toml"
```

The config file must be owned by you or root and must not be writable by group or
others. Unknown keys are errors. Component overrides may change paths and policy only.

The release workflow signs artifacts with GitHub OIDC and publishes the attestation
bundle to GitHub. The verifier reads the expected signer workflow from the config and
requires the SLSA provenance predicate and GitHub identity described in
[verify-release.md](verify-release.md). No external GitHub CLI is needed on nodes; the
producer workflow publishes the release and its attestation separately.

## Check and install

```sh
tuishou verify reiki-v0.3.2-x86_64-unknown-linux-musl.tar.gz \
  --repo ddproxy/reiki --tag v0.3.2
tuishou verify lu-v0.4.1-x86_64-unknown-linux-musl.tar.gz \
  --repo ddproxy/tuishou-lu --tag v0.4.1
tuishou install reiki
tuishou install lu
tuishou status
```

For a test install without a signed release, `--allow-unattested` warns and continues.
It is an explicit local opt-in; auto-update always refuses it.

## On-node updates

Install `tuishou` on each node, then run `tuishou update` there. Each node verifies in
process; make sure its config and any `GITHUB_TOKEN` needed for `api.github.com` are
present. Trust-root refresh may need access to `tuf-repo-cdn.sigstore.dev`, or pin
`[verify].trust_root` to an operator-managed file. See [node-update.md](node-update.md)
for the procedure and for fleet-wide settings.

## Safe practice

- Keep `.github/workflows/release.yml` fixed; change it only with a reviewed PR.
- Use `cache_ttl` to bound TUF refresh frequency.
- Check the reported identity, workflow, ref, and log index from `tuishou verify`.
- Upgrade one node first, then update the rest.
