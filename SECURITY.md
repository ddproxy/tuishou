# Security Policy

## Supported versions

Security fixes are applied to the latest release. Earlier releases are not maintained.

## Reporting a vulnerability

Report security issues privately to the maintainers, for example through a private security advisory. Do not open a public issue for an unpatched vulnerability.

Include:

- A description of the issue and impact
- Reproduction steps or proof of concept
- Affected versions or commit

## Scope

In scope: the shell installer, the `tuishou` binary and its in-process attestation verifier, config trust checks, state files, checksum handling, rollback behavior, token handling and the HTTP path.

## Out of scope

- Platforms the installer does not support, and manual installs of unverified targets.
- An explicit `--allow-unattested` downgrade, or a compromised GitHub account, workflow, release asset or upstream dependency.
- Containers, source builds, and Windows or macOS installers.
- Host misconfiguration, vulnerable third-party services, and behavior of components outside this repository.
- `tuishou top`, which is not implemented in this build. Auto-update is opt-in, off by default and not recommended for production.

## Verification and bootstrap caveat

The shell bootstrap checks the SHA-256 digest before a verifier exists; that check alone does not prove who built the archive. After installation the downloaded binary's `verify` command checks the archive's GitHub attestation in process. A refusal removes the new binary, restores `tuishou.prev` when present, and exits 4. Verification is fail closed: component installs continue past an attestation failure only when the operator explicitly passes `--allow-unattested`, and auto-update refuses that flag. There is no offline installer attestation. See [docs/security.md](docs/security.md) and [docs/limits.md](docs/limits.md).

## Privileges and tokens

The installer and binary never invoke `sudo` or elevate privileges. An unwritable prefix produces a root rerun command for the operator to run deliberately.

Tokens are read only from `GITHUB_TOKEN` and `GH_TOKEN`. They are sent only to GitHub API endpoints, stripped on cross-origin redirects and redacted from logs and errors. Release asset downloads carry no token, and no token value is printed.

## Disclosure expectations

Reports are handled privately. Details are coordinated with the reporter and disclosed after a fix is available. Do not publish reproduction details for an unpatched issue.
