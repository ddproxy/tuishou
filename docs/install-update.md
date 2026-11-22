# Install and update

## GitHub source

```toml
[install]
method = "copy"
source = "github"
repo = "owner/name"
asset = "tool-{version}-{target}.tar.gz"

[verify]
signer_workflow = ".github/workflows/release.yml"
```

`tuishou install` resolves the version, downloads the asset, and verifies its
GitHub attestation in process before installation. Verification checks the artifact
digest, Sigstore/Fulcio chain, Rekor SET and inclusion proof, certificate validity at
signing time, and the GitHub OIDC workflow identity and SLSA provenance. A TUF refresh
may contact `tuf-repo-cdn.sigstore.dev`. Details are in
[verify-release.md](verify-release.md).

If verification fails or no attestation exists, the install is refused with exit 1.
`--allow-unattested` warns and continues after an explicit opt-in; it is intended only
for local testing of unattested builds and is rejected in `auto` mode.

## Local source

```toml
[install]
method = "copy"
source = "local"
path = "./dist/tool.tar.gz"
```

Local files are not attested by this flow; use `--allow-unattested` only when you trust
the file out of band.

## Update

```sh
tuishou update
tuishou update --version v1.2.3
tuishou self-update
```

Each command applies the same verification rules before replacing files. `tuishou`
uses `GITHUB_TOKEN`/`GH_TOKEN` only for `api.github.com`; asset downloads use the
configured host allowlist and never receive the token.

## Rollback and receipts

`tuishou rollback` uses saved receipts; `tuishou status` shows installed versions and
update state. Receipts record the digest and verification decision.
