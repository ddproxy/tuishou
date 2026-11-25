# Attestation fixtures

These fixtures are real, upstream-signed Sigstore material. They are used only for
negative tests: the test suite proves `tuishou` refuses malformed bundles, message
signatures without a DSSE envelope, wrong workflow identities, unsupported payload
types and predicates, and bundles missing verification material. No test accepts one
of these fixtures as a valid `tuishou` release.

They are copied from official upstream crates, not synthesized locally. Each file's
SHA-256 is pinned below; the negative tests assert content that depends on those files.

| File | SHA-256 | Upstream source |
| --- | --- | --- |
| `npm_slsa_bundle_v03.json` | `8900ee3e85bcf5ac7e49c21810b6b7503793718868189ddfa5299eccd79b46c7` | `sigstore` 0.15.0, `tests/data/npm_slsa_bundle_v03.json` |
| `a.txt` | `632579b5183d418ff3dc443994fd35e0e1a2a6ce89a1ee22fdca577f8e8bc9c3` | `sigstore_protobuf_specs` 0.5.2, `assets/a.txt` |
| `a.txt.sigstore` | `4ac8d6314fede1aef32ffcdb586895b58330bc6265089c94ce2633d521391b27` | `sigstore_protobuf_specs` 0.5.2, `assets/a.txt.sigstore` |

`npm_slsa_bundle_v03.json` is a genuine SLSA-v0.3 DSSE bundle. Its workflow identity,
repository, and predicate are not a `tuishou` release, so the strict identity and
predicate policy refuses it. `a.txt.sigstore` is a genuine message-signature bundle
without a DSSE in-toto payload, so it is refused as an unsupported payload.

## Fetch and copy commands

The upstream crates were fetched from crates.io and extracted into a scratch directory:

```sh
cd "$(mktemp -d)"
curl -fsSL -o sigstore-0.15.0.crate \
  https://static.crates.io/crates/sigstore/sigstore-0.15.0.crate
curl -fsSL -o sigstore_protobuf_specs-0.5.2.crate \
  https://static.crates.io/crates/sigstore_protobuf_specs/sigstore_protobuf_specs-0.5.2.crate
mkdir sigstore sigstore_protobuf_specs
tar -xzf sigstore-0.15.0.crate -C sigstore
tar -xzf sigstore_protobuf_specs-0.5.2.crate -C sigstore_protobuf_specs

cp sigstore/sigstore-0.15.0/tests/data/npm_slsa_bundle_v03.json \
  tests/fixtures/attestation/
cp sigstore_protobuf_specs/sigstore_protobuf_specs-0.5.2/assets/a.txt \
  tests/fixtures/attestation/
cp sigstore_protobuf_specs/sigstore_protobuf_specs-0.5.2/assets/a.txt.sigstore \
  tests/fixtures/attestation/
```

Verify the copies in the repository root:

```sh
sha256sum tests/fixtures/attestation/*
```

## GitHub release attestation fixture

The preferred positive fixture is a real `tuishou` release artifact plus the bundle
returned by GitHub's attestations API. It could not be fetched in the implementation
environment: outbound requests to `api.github.com` and the Sigstore TUF CDN were
blocked by the sandbox proxy (`CONNECT tunnel failed, response 403`). No substitute
claim is made that a live GitHub bundle or a live TUF refresh was exercised.

With network access, a release bundle would be fetched as follows. Never put a token
on the command line; use the environment and print only the digest and bundle path.

```sh
export GITHUB_TOKEN=...  # or GH_TOKEN; needs attestations:read on the repo
ASSET=tuishou-v0.2.0-x86_64-unknown-linux-musl.tar.gz
DIGEST=$(sha256sum "$ASSET" | cut -d' ' -f1)
curl -fsS \
  -H 'Accept: application/vnd.github+json' \
  -H "Authorization: Bearer ${GITHUB_TOKEN}" \
  "https://api.github.com/repos/ddproxy/tuishou/attestations/sha256:${DIGEST}" \
  -o "tests/fixtures/attestation/tuishou-bundle.json"
tuishou verify "$ASSET" --repo ddproxy/tuishou \
  --signer-workflow .github/workflows/release.yml --tag v0.2.0
```

A checked-in positive fixture must be accompanied by its artifact digest, the exact
fetch command above with the concrete digest and tag, and a negative test that changes
the digest or workflow and expects refusal.
