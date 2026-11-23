# Limits

For planners: supported targets, the bootstrap trust boundary and non-goals.

## Supported installer target

The shell installer supports Linux x86_64 only: `x86_64-unknown-linux-musl`. On any other platform it prints `ERROR: unsupported platform: <os>/<arch>`, lists the release targets that exist, and exits 3 before changing the host.

The binary has code paths for four targets. The release workflow currently ships x86_64 Linux only:

| Target | Status |
| --- | --- |
| `x86_64-unknown-linux-musl` | Installed by the shell installer; shipped by the release workflow. |
| `aarch64-unknown-linux-musl` | Code path and verified target; not shipped by the release workflow without a cross-linker. |
| `x86_64-apple-darwin` | Code path only; not shipped, not verified. |
| `aarch64-apple-darwin` | Code path only; not shipped, not verified. |

Manual install for another target is outside the installer's scope. The manual path uses the same checksum and `tuishou verify` checks, but no archive is claimed as supported for an unverified target unless the release workflow published one. See [install.md](install.md).

## Bootstrap trust boundary

Before `tuishou` exists on the host, the shell can check only the SHA-256 digest from the `.sha256` release asset against the downloaded archive. Both assets come from the same release. A checksum-only check ties the archive to that digest; it does not prove who built the archive. This caveat is the reason the installer does not claim attestation success at the checksum step.

After the checksum passes, the installer extracts and stages `tuishou`, then runs the downloaded binary's own command:

```sh
tuishou verify <archive> --repo ddproxy/tuishou --tag <tag>
```

That step checks the GitHub attestation: OIDC issuer, signer workflow, tag ref, Rekor inclusion, source repository, ref and digest. If it fails, the installer removes the new binary, restores `tuishou.prev` when one exists, and exits 4. Automatic verification needs API access; there is no offline installer attestation.

## Fail-closed behavior and exit codes

Verification refusals stop the operation. No unverified binary is left at the install destination. Component installs also fail closed on attestation unless the operator passes `--allow-unattested` explicitly; auto-update refuses that flag.

Installer exit codes:

| Code | Meaning |
| --- | --- |
| `0` | Success. |
| `2` | Usage error, including an unknown component. |
| `3` | Unsupported platform. |
| `4` | Verification failed: checksum or attestation. |
| `5` | Install failed: metadata, download, prefix or state. |

The `tuishou` binary uses `0` ok, `1` error, `2` usage, and `10` updates available for `update --check`.

## Non-goals

- The installer never invokes `sudo`. If the prefix is not writable it prints the exact command to rerun as root.
- It does not install or enable system services, manage component configuration, or manage databases.
- `tuishou auto-update install-timer` prints systemd units; it does not install or enable them.
- Remote fleet SSH is out of scope. Removed commands and config sections are not part of the installer.
- `tuishou top` does not exist in this build. The installer prints it as planned in its next-steps block; do not automate it.
- No offline installer attestation: the verifier is the artifact being installed.
- Auto-update is opt-in, off by default, stable-only and not recommended for production.
- One rollback copy per binary (`<name>.prev`) is kept. It is not a backup system.
- Containers, source builds, Windows and macOS installers are out of scope.

Exact messages and fixes are in [troubleshooting.md](troubleshooting.md). Verification scope is in [security.md](security.md).
