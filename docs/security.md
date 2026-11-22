# Security model

## Release verification

`tuishou` verifies release artifacts against GitHub attestations in process using the
official `sigstore` Rust crate (Apache-2.0) and GitHub's attestations API. Verification
is fail closed: a digest mismatch, malformed bundle, untrusted chain, missing Rekor
proof, wrong OIDC issuer or workflow identity, wrong source ref/digest, unexpected
predicate, or any internal error stops the install with exit 1. `--allow-unattested`
explicitly warns and continues for local use, but auto-update refuses it. See
[verify-release.md](verify-release.md).

No subprocess is executed for attestation verification; there is no dependency on an
external GitHub CLI binary or CLI login. The source contains a test that fails if an
external CLI subprocess is reintroduced.

## Credentials and network

- `GITHUB_TOKEN` or `GH_TOKEN` is the only token source. It is sent only to
  `api.github.com`, is stripped on redirects, and is never logged.
- Release asset downloads use the configured host allowlist and receive no token.
- Trust-root refresh contacts only `tuf-repo-cdn.sigstore.dev`; bundle verification
  itself runs offline against the embedded or cached root.
- HTTP uses fixed timeouts, response size caps, and bounded redirects. An oversized
  attestation response is rejected rather than buffered without limit.

## Trust roots

The embedded Sigstore trusted root and TUF root are pinned and their checksums are
verified at runtime. TUF refresh updates the cache under a `0700` directory. Setting
`[verify].trust_root` to a path makes verification use that operator-managed file
directly with no refresh; protect that file accordingly.

## Config trust

`tuishou.toml` is refused unless it is owned by the running user or root and is not
writable by group or others. Unknown keys are errors. Component overrides may change
paths and policy, but never the repository or asset host; downloads come only from
GitHub release assets.

## Host execution

`install`, `update`, and local service operations run commands on the host where
`tuishou` is executed. Updates are performed by each node locally; no commands are sent
to nodes from a central host. Only use trusted configs. `tuishou` does not elevate
privileges for you; run it with the account that should own the files and services.

## Receipts

Applied updates write receipts under the state directory with artifact digest, version,
and decision. They record the local result and are not a substitute for the attestation
itself.
