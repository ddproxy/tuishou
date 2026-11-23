# Verify a release

`tuishou` verifies downloaded release artifacts against their GitHub attestations in
process. No external GitHub CLI, CLI login, or shelling out is involved.

Verification is **fail closed**: a download is installed only when verification succeeds.
If verification cannot be performed or refuses the artifact, installation stops with
exit 1. `--allow-unattested` explicitly changes that to a warning for local use; it is
rejected in `auto` mode.

## What is verified

For each downloaded asset, `tuishou`:

1. Computes its SHA-256 digest and requires a bundle whose in-toto subject digest
   matches it exactly. Bundles are fetched from the GitHub attestations API at
   `/repos/OWNER/REPO/attestations/sha256:DIGEST`.
2. Verifies the bundle's Sigstore certificate chain against the Fulcio roots in the
   embedded trust root, or against a TUF-refreshed root when no local trust-root file is
   configured. Certificate validity is checked at the bundle's signing time.
3. Verifies the Rekor signed entry timestamp and Merkle inclusion proof for the bundle.
4. Checks the OIDC issuer is `https://token.actions.githubusercontent.com` and that
   the identity's `workflow` SAN matches the expected owner/repo, the configured
   signer workflow (default `.github/workflows/release.yml`), and the expected tag ref.
5. Checks the source repository, ref, and source digest certificate extensions, and
   accepts only in-toto SLSA Provenance v1 or v0.2 predicates.
6. Requires the bundle's DSSE envelope to use `application/vnd.in-toto+json` and to
   cover the matching artifact subject.

Any other issuer, identity, predicate, payload type, ref, workflow path, missing
subject, missing verification material, malformed bundle, oversized response, or
verification error is refused with a specific error. No fallback accepts a bundle that
failed verification.

## Trust roots and cache

By default, `tuishou` ships a pinned Sigstore trusted root and TUF root. It reuses a
fresh cached root when available, otherwise refreshes via TUF and falls back to the
embedded pinned root if refresh fails. Only the official TUF repository at
`tuf-repo-cdn.sigstore.dev` is used for refresh; the cache directory is created with
`0700` permissions. Configure it in `tuishou.toml`:

```toml
[verify]
trust_root = "embedded"     # or a path to an operator-managed trusted_root.json
cache_dir = "/var/cache/tuishou/verify"
cache_ttl = "24h"
```

A path in `trust_root` is trusted operator input: it is loaded directly and never
refreshed over the network. `cache_ttl` accepts Go-style durations such as `30m`,
`24h`, or `168h`; the default is `24h`. Verification always uses offline Sigstore
checking for the bundle itself; only trust-root refresh can contact the TUF CDN.

## Network and credentials

Requests use `tuishou`'s HTTP rules: fixed timeouts, redirects limited to the same
configured allowlist, and response size caps. A token is read only from `GITHUB_TOKEN`
or `GH_TOKEN`; it is sent only to `api.github.com` and is never logged. GitHub's
attestations API may require the `attestations:read` scope. Asset downloads use the
configured host allowlist and never receive the token.

## Per-component overrides

Global verification settings can be overridden for a component:

```toml
[verify.components.tuishou]
signer_workflow = ".github/workflows/release.yml"

[verify.components.reiki]
signer_workflow = ".github/workflows/release.yml"

[verify.components.lu]
signer_workflow = ".github/workflows/release.yml"
```

An override changes only the expected signer workflow for that component; it does not
weaken any cryptographic or identity check.

## Inspect a bundle manually

`tuishou verify` reports the checks above for a local bundle and artifact:

```sh
tuishou verify artifact.tar.gz \
  --repo owner/name \
  --signer-workflow .github/workflows/release.yml \
  --tag v1.2.3
```

`--tag` is required: it is resolved to a release commit and checked against the
certificate source digest as well as the SAN, which must be a `refs/tags/...` ref.
Exit status is 0 when verified, 1 when refused, and 2 for usage errors.

As an **optional cross-check only**, the GitHub CLI can be used separately if it is
installed and authenticated; this is never invoked by `tuishou` and is not required:

```sh
gh attestation verify artifact.tar.gz \
  --repo owner/name \
  --signer-workflow .github/workflows/release.yml
```

## Known advisories in the verifier's dependency tree

The verifier uses the `sigstore` crate. Two advisories reach tuishou through that crate's OIDC signing dependencies, which tuishou never calls (it only verifies):

- RUSTSEC-2023-0071, the Marvin timing attack on RSA private-key operations. Tuishou performs no RSA private-key operation.
- RUSTSEC-2026-0543, chrono is unmaintained. It is a maintenance notice, not a vulnerability.

Both are ignored in `deny.toml` with the reasons recorded next to them. They are reviewed whenever `sigstore` is updated, and a rewrite of the verifier on a few audited primitives is planned so that the large dependency tree and these exceptions can go. Any other advisory still fails the audit check.
