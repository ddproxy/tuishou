# Install and update

## Built-in components

`tuishou` installs only the built-in components listed in [components.md](components.md).
Repositories and asset hosts are fixed; a config override may change paths and policy
but cannot redirect downloads.

```toml
[verify]
signer_workflow = ".github/workflows/release.yml"

[auto_update]
enabled = false
```

`tuishou install` resolves the version, downloads the asset, verifies its SHA-256
checksum and GitHub attestation in process, then installs the expected binaries
atomically. Verification checks the artifact digest, Sigstore/Fulcio chain, Rekor SET
and inclusion proof, certificate validity at signing time, and the GitHub OIDC workflow
identity and SLSA provenance. A TUF refresh may contact `tuf-repo-cdn.sigstore.dev`.
Details are in [verify-release.md](verify-release.md).

If verification fails or no attestation exists, the install is refused with exit 1.
`--allow-unattested` warns and continues after an explicit opt-in; it is intended only
for local testing of unattested builds and is rejected by auto-update.

## Install

```sh
tuishou install reiki
tuishou install lu --version v0.4.0 --prefix /opt/tuishou/bin
tuishou install all
```

`install all` attempts each default component in table order. A component whose
expected binaries are absent from the latest release is reported as
`not in this release`; it is not a failure. Every component gets exactly one report.

## Update and check

```sh
tuishou update --check
tuishou update
tuishou update reiki
tuishou self-update
```

A bare `tuishou update` means `update all`. Only installed components are updated by
that path; a component that is not installed is reported as `not installed` and is
installed only by an explicit `install <name>` or `update <name>`.

`update --check` shares its evaluation with `status` and `auto-update`:

- exit 0: nothing installable is pending
- exit 10: at least one installed component has an installable update
- exit 1: an evaluation error occurred
- exit 2: usage error, including an unknown component name

Every component is reported even when another one fails.

## Rollback and receipts

`tuishou rollback` restores the previous binary. `tuishou status` shows the same fixed
vocabulary used by `update` and `auto-update`. Receipts record the digest and
verification decision.
