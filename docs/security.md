# Security model

For reviewers: verification scope, trust boundaries and bootstrap limits.

## Bootstrap trust boundary

Before `tuishou` exists on the host, the installer can check digests but cannot verify an attestation. The `.sha256` asset comes from the same release as the archive, so a check against it proves consistency between two files from the same untrusted channel; it does not prove who built the archive and is not a trust bootstrap.

The out-of-band root of trust must come from outside this repo: release records, a signed note or a configuration-management record. Pass its 64-hex digest as `--expect-sha256` (case-insensitive). The installer compares it to the downloaded archive immediately, before extraction, and exits 4 on invalid format or mismatch. The release `.sha256` is still checked afterwards as a secondary consistency check.

```sh
BOOTSTRAP_SHA256=$(awk '{ print $1; exit }' /etc/tuishou/bootstrap.sha256)
curl -fsSL https://raw.githubusercontent.com/ddproxy/tuishou/main/install.sh | bash -s -- --components reiki,lu --expect-sha256 "$BOOTSTRAP_SHA256"
```

After both checks pass, the installer validates the tar listing and extracts only a single regular `tuishou` entry into a private staging directory. It then runs that staged binary's own `verify` command on the archive:

```sh
tuishou verify <archive> --repo ddproxy/tuishou --tag <tag>
```

That command checks the GitHub attestation in process. The destination binary is neither executed nor replaced before this check succeeds; on failure it stays untouched and the installer exits 4. The prefix is refused when a path component is a symlink or the prefix or binary directory is foreign-owned or writable by group or others. The previous version comes from local state, never from executing an existing destination. Downloads, tar listings and extracted entries are size-bounded. The bootstrap caveat remains: the verifier is the artifact being installed, so in-process attestation cannot verify the verifier. There is no offline installer attestation. Pinning is optional in this build; mandatory pinned digests and signed packages are a residual design decision. See [limits.md](limits.md).

## Trust boundaries

| Boundary | What crosses it | Trust rule |
| --- | --- | --- |
| GitHub release API and assets | Tag metadata, archives, `.sha256` files and attestation bundles | Metadata comes from the GitHub API; asset downloads carry no token. The digest and attestation decide whether an artifact is accepted. |
| Shell installer | `install.sh` fetched over https | Resolves a release, checks an optional out-of-band pinned digest before extraction, checks the release `.sha256` as a consistency check, validates the archive entry, stages the binary in a private directory and runs its `verify` command before touching the destination. It refuses unsafe prefixes, never invokes `sudo`, and prints a root rerun command for an unwritable prefix. |
| Verifier binary | The installed `tuishou` executable | Its `verify` command checks the archive's attestation in process; no external CLI or login is used. Component installs fail closed unless the operator passes `--allow-unattested` explicitly; auto-update refuses that flag. |
| Config file | `tuishou.toml` | Refused unless owned by the running user or root and not writable by group or others. Unknown keys are errors. Repositories and asset hosts cannot be overridden. |
| Host execution | Local install, update and service commands | Runs with the privileges of the invoking account; no privilege elevation and no fleet SSH. Only trusted configs should be used. |
| State and cache | Install state, auto-update state and the trusted-root cache | Local records of versions and decisions; not a substitute for attestation. The cache directory is `0700` and the cached root file is `0600`. |

## Release verification

`tuishou` verifies release artifacts against GitHub attestations in process using the
official `sigstore` Rust crate (Apache-2.0) and GitHub's attestations API. Verification
is fail closed: a digest mismatch, malformed bundle, untrusted chain, missing Rekor
proof, wrong OIDC issuer or workflow identity, wrong source ref/digest, unexpected
predicate, or any internal error stops the install with exit 1. `--allow-unattested`
explicitly warns and continues for local use, but auto-update refuses it. Both stable
and dev channels require the release workflow identity on `refs/tags/<tag>`; dev
builds are dispatched on their tag ref so the identity matches. See
[verify-release.md](verify-release.md).

No subprocess is executed for attestation verification; there is no dependency on an
external GitHub CLI binary or CLI login. The source contains a test that fails if an
external CLI subprocess is reintroduced.

## Self-update boundary

With `[auto_update] self = true`, auto-update may replace its own executable. The
candidate `tuishou` release uses the same checksum and in-process attestation checks,
including the workflow identity resolved for `tuishou` on `refs/tags/<tag>`; stable and
dev channels do not weaken that identity check. The staged binary's `--version` output
is checked before any installed file is replaced. A failed staged check leaves the
installed binary untouched and marks the tag bad.

Commit writes a private temp file beside the destination, fsyncs it, renames it
atomically and keeps the previous binary as `<binary>.prev`. The next scheduled pass
runs `--version` again before hold or fleet gates, restores `<binary>.prev` and marks
the tag bad when the new binary cannot start or reports a different tag. Only
`tuishou-auto-update.timer` is restarted; the running auto-update service is never
restarted from its own pass.

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

## Hosted TUIs

`tuishou top` spawns the `reiki` and `lu` TUI children. Config overrides are
argument vectors, never shell commands. Child executables are resolved through
the safe program resolver and vetted before spawn; state never supplies
executable argv. Children start with a cleared environment plus a small
allowlist, so updater tokens are not inherited. TUI state is scoped per config
and prefix, is opened without following symlinks, and is accepted only with a
trusted owner and mode. Child-derived text is sanitized before it reaches the
host footer. See [top.md](top.md).

## Receipts

Applied updates write receipts under the state directory with artifact digest, version,
and decision. They record the local result and are not a substitute for the attestation
itself.
