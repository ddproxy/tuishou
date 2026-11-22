# FAQ

## Do I need the GitHub CLI installed?

No. `tuishou` verifies attestations in process. It does not spawn an external CLI,
require an external CLI login, or require any GitHub CLI on PATH. The only manual
external cross-check documented in this repository is an optional command in
[verify-release.md](verify-release.md); it is never invoked by `tuishou`.

## What happens if verification fails?

The install, update, self-update, or auto-update is refused with exit 1 and a specific
reason: digest mismatch, wrong issuer or workflow, missing Rekor proof, untrusted
chain, malformed bundle, and so on. `--allow-unattested` turns failures into warnings
for explicit local use. Auto-update always refuses `--allow-unattested`.

## Which components are built in?

`reiki`, `lu` and `tuishou`. See [components.md](components.md). A bare `tuishou update`
checks all three. An unknown component name is a usage error (exit 2) that lists the
known names.

## What makes a config file acceptable?

`tuishou.toml` must be owned by you or root and must not be writable by group or
others. Unknown keys are errors. A component override can change paths and policy, but
it cannot change the repository or asset host: downloads come only from GitHub release
assets.

## Which verifier does tuishou use?

The official `sigstore` Rust crate, pinned to `=0.15.0`, with its `rustls-tls` feature.
It is used in strict offline bundle-verification mode with a pinned/persisted trust
root; `tuishou` adds explicit Rekor SET and Merkle inclusion checks, GitHub OIDC
identity checks, SLSA provenance predicate checks, and its own HTTP/token rules.

## Which hosts are contacted?

- `api.github.com` for releases and attestations, with a token only when configured.
- `github.com` and release-asset hosts from the allowlist for asset downloads, with no
  token.
- `tuf-repo-cdn.sigstore.dev` for optional trust-root refresh, unless a local
  `[verify].trust_root` is configured.

## Can I pin the trust root?

Yes. Set `[verify].trust_root` to a path containing a Sigstore `trusted_root.json`.
That file is trusted input and is never refreshed. The embedded roots are pinned and
checksum-checked at runtime.

## What identities are accepted?

Only `https://token.actions.githubusercontent.com` with a workflow SAN for the
expected owner/repo, the configured signer workflow (default
`.github/workflows/release.yml`), and the expected `refs/tags/...` ref, plus matching
source repository/ref/digest extensions. Everything else is refused.

## How do I inspect a bundle?

```sh
tuishou verify artifact.tar.gz --repo owner/name --tag v1.2.3
```

It prints the digest, issuer, identity, workflow, ref, and Rekor log index. Exit 0
means verified, 1 refused, 2 usage.
