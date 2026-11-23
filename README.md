# Tuishou

[![CI](https://github.com/ddproxy/tuishou/actions/workflows/ci.yml/badge.svg)](https://github.com/ddproxy/tuishou/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/ddproxy/tuishou)](https://github.com/ddproxy/tuishou/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Status: stable for the shell installer on Linux x86_64; this text describes tuishou v0.2.1. Scheduled auto-update is opt-in and off by default.

## What and why

Tuishou installs and updates release artifacts for the Lu stack: `tuishou` itself, `reiki` and `lu`. It downloads from built-in GitHub repositories, checks SHA-256 checksums, verifies GitHub build attestations in process with Sigstore, and records what it installed. No `gh` CLI is used. Component code lives in [ddproxy/reiki](https://github.com/ddproxy/reiki) and [ddproxy/tuishou-lu](https://github.com/ddproxy/tuishou-lu); see [components](docs/components.md).

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/ddproxy/tuishou/main/install.sh | bash -s -- --components reiki,lu
```

Expected first lines:

```text
PASS: checksum: PASS (tuishou-<tag>-x86_64-unknown-linux-musl.tar.gz)
PASS: attestation: PASS (tuishou verify tuishou-<tag>-x86_64-unknown-linux-musl.tar.gz)
```

The installer verifies the checksum before it has a verifier, then runs the binary it just installed to verify its own archive. It removes the new binary and exits 4 if that attestation check fails. It never calls `sudo`; if the prefix is not writable it prints the exact command to rerun as root. See [install](docs/install.md).

To inspect before running:

```sh
curl -fsSLO https://raw.githubusercontent.com/ddproxy/tuishou/main/install.sh
less install.sh
bash install.sh --help
bash install.sh --components reiki,lu
```

Manual alternative: download the release archive and its `.sha256` file, run `sha256sum -c` on the checksum file, extract `tuishou`, then run `tuishou verify <archive> --repo ddproxy/tuishou --tag <tag>` before putting the binary on `PATH`. The manual path executes no release code until after the checksum passes; it does not check the attestation until `tuishou verify` runs.

## Quickstart (10 minutes)

1. Install and verify the stack. Runs as a normal user install into `~/.local`; root installs into `/usr/local`, which is already on `PATH`.

   ```sh
   curl -fsSL https://raw.githubusercontent.com/ddproxy/tuishou/main/install.sh | bash -s -- --components reiki,lu
   # expect: PASS: attestation: PASS (tuishou verify
   ```

2. Put the user install on `PATH` (skip when the installer used `/usr/local/bin`).

   ```sh
   export PATH="$HOME/.local/bin:$PATH"
   ```

3. Proof: show one fixed-vocabulary line per component. `tuishou status` reads local state and checks the APIs; it is the checkable result of this guide.

   ```sh
   tuishou status
   # expect: reiki: up to date (
   ```

   Expected shape, with real versions and timestamps:

   ```text
   reiki: up to date (v<tag>) | auto_update=false installed=v<tag> latest=v<tag> last_check=never last_result=never bad=-
   lu: up to date (v<tag>) | auto_update=false installed=v<tag> latest=v<tag> last_check=never last_result=never bad=-
   tuishou: up to date (v<tag>) | auto_update=false installed=v<tag> latest=v<tag> last_check=never last_result=never bad=-
   ```

## How it works

```mermaid
flowchart LR
  A[GitHub release API] --> B[archive + .sha256]
  B --> C[shell installer checks SHA-256]
  C --> D[staged tuishou binary]
  D --> E[tuishou verify checks its own archive]
  E --> F[tuishou install reiki, lu]
  F --> G[state: installed versions and rollback copies]
```

The installer resolves the latest stable tag (or `--version`), downloads the archive and checksum into a private temporary directory under the prefix, verifies the digest, extracts only `tuishou`, stages it with a rename, keeps `<name>.prev` for rollback, then calls `tuishou verify` and `tuishou install`. Components are verified again inside `tuishou install`. Details: [architecture](docs/architecture.md).

## Configuration

`tuishou` reads `./tuishou.toml` by default; `--config <path>` or `TUISHOU_CONFIG` overrides it. Unknown keys and group/world-writable config files are rejected. Installer flags are described in [install](docs/install.md); CLI flags and environment variables are in [cli](docs/cli.md).

| Key | Default | Meaning |
| --- | --- | --- |
| `[auto_update] enabled` | `false` | No scheduled update runs until this is true. |
| `prefix` | unset | Install root for binaries, state and verify cache; CLI `--prefix` wins. |
| `interval` | `10m` | Timer interval; minimum `5m`. |
| `components` | `reiki, lu, tuishou` | Components considered by auto-update; `[[component]] auto_update` still opts each one in. |
| `policy` | `patch` | Version boundary: `patch`, `minor` or `any`. |
| `channel` | `stable` | Stable releases only; `release` is a legacy alias. |
| `min_release_age` | `10m` | Delay before a new release is eligible. |
| `require_attestation` | `true` | Always true; `false` is a config error. |
| `controller_policy` | `manual` | `manual`, `when_idle` or `window`; non-manual requires `backup_dir`. |
| `backup_dir`, `backup_keep` | unset, `5` | Controller database backup location and retention. |
| `reiki_health_url` | `http://127.0.0.1:8080/health` | Health endpoint checked after a Reiki restart. |
| `drain_timeout`, `health_timeout`, `command_timeout` | `30m`, `5m`, `30s` | Update and child-process deadlines. |
| `verify.signer_workflow` | `.github/workflows/release.yml` | Expected attestation signer identity. |
| `verify.cache_ttl` | `24h` | Trusted-root cache lifetime before TUF refresh. |

Minimal example (valid):

```toml
[auto_update]
enabled = true
```

Full example (valid):

```toml
[auto_update]
enabled = true
prefix = "/usr/local/bin"
interval = "10m"
components = ["reiki", "lu", "tuishou"]
policy = "patch"
min_release_age = "10m"
channel = "stable"
max_version = "v9.9.9"
require_attestation = true
controller_policy = "manual"
backup_keep = 5
drain_timeout = "30m"
controller_wait_timeout = "30m"
health_timeout = "5m"
heartbeat_max_age = "5m"
command_timeout = "30s"
agent_unit = "ting.service"
controller_unit = "lu-controller.service"
reiki_health_url = "http://127.0.0.1:8080/health"
notify_after_failures = 3

[update]
start_delay_max = "0"

[verify]
signer_workflow = ".github/workflows/release.yml"
trust_root = "embedded"
cache_ttl = "24h"
```

The full reference is [configuration](docs/configuration.md).

## Operating it

| Task | Command | Note |
| --- | --- | --- |
| Check for updates | `tuishou update --check` | Exit 10 means updates exist, 1 means a failed check. |
| Update components | `tuishou update` | Updates installed components; explicit `tuishou update reiki` can install a missing one. |
| Roll back | `tuishou rollback reiki` | Restores `<name>.prev`. The installer keeps `tuishou.prev` for the same purpose. |
| Inspect attestations | `tuishou verify <archive> --repo ddproxy/tuishou --tag <tag>` | Uses the in-process Sigstore verifier. |
| Auto-update status | `tuishou auto-update status --json` | Reports last check, holds and bad versions. |
| Logs | `journalctl -u reiki.service` | Structured `tuishou:` lines are emitted on stderr and captured by systemd. |

To enable auto-update: set `[auto_update] enabled = true` in `tuishou.toml`, print the units with `tuishou auto-update install-timer --interval 10m`, then install those units on the host. `install-timer` prints units; it does not write or enable them. Auto-update is fail-closed on attestation and is not recommended for production. See [operations](docs/operations.md) and [auto-update](docs/auto-update.md).

## Security model

What verification covers: the SHA-256 checksum ties the downloaded archive to the checksum asset; `tuishou verify` checks the GitHub Actions issuer, signer workflow path, tag ref, Rekor inclusion and (on install) the release source repository, ref and commit digest. Component installs fail closed on attestation unless `--allow-unattested` is explicitly passed, which never affects checksums. Auto-update refuses that flag.

What it does not cover: the shell bootstrap can only check the checksum before a verifier exists, so it runs the downloaded binary's `verify` command before the attestation result is known. Verification does not make a compromised GitHub account, workflow or dependency safe, and it is not a sandbox. No token is printed; `GITHUB_TOKEN` and `GH_TOKEN` are sent only to GitHub API hosts and redacted from errors. See [security](docs/security.md) and [SECURITY.md](SECURITY.md).

## Compatibility matrix

| Layer | Target or client | Last checked |
| --- | --- | --- |
| Shell installer | Linux x86_64, `x86_64-unknown-linux-musl` | v0.2.1, 2026-11-22 |
| Shell installer | Linux aarch64, macOS, other platforms | unsupported; manual install only |
| tuishou binary | `x86_64-unknown-linux-musl` (verified target) | v0.2.1, 2026-11-22 |
| tuishou binary | `aarch64-unknown-linux-musl` (verified target, built only with a cross-linker) | code path, 2026-11-22 |
| tuishou binary | `x86_64-apple-darwin`, `aarch64-apple-darwin` (listed, unverified, not built by the release workflow) | code path, 2026-11-22 |
| Components | `reiki` from `ddproxy/reiki`, `lu` from `ddproxy/tuishou-lu` | asset naming checked against source, 2026-11-22 |
| Test harness | bash, curl, tar, sha256sum, perl or python3 | installer test, 2026-11-22 |

## Limits and non-goals

- The installer targets Linux x86_64 only. It prints the existing release targets and exits 3 elsewhere.
- It never invokes `sudo`, does not install system services, and does not manage component configuration or databases.
- Only one rollback copy per binary is kept; it is not a backup system.
- `tuishou top` does not exist in this build. The installer prints it as a planned command in its next-steps block; do not automate it.
- Attestation needs API access and is not cached offline by the installer. Normal release binaries ignore the test-only loopback and fake-verifier hooks.
- Auto-update is opt-in, stable-only, and not recommended for production. Prerelease channels are rejected.
- Containers, source builds, Windows and macOS installers are out of scope. See [limits](docs/limits.md).

## FAQ

Q: Why does the installer print `bootstrap: only the checksum can be checked before tuishou exists`?
A: A SHA-256 digest is self-contained. Attestation verification needs the verifier binary, so it runs immediately after installation; failure removes the binary.

Q: Can I install without components?
A: Yes: `bash install.sh --no-components`. `tuishou` is always installed.

More questions: [faq](docs/faq.md).

## Contributing and development

```sh
make help
make build
make test
make lint
make docs-check
make release-check
```

`make test` runs workflow validation, the fake-server installer test and the Rust suite. `make docs-check` executes every quickstart command in a throwaway directory, validates both config examples with the real validator, and checks links, style and flag drift. CI runs the same checks. Tagged `v*` releases are built and attested by the release workflow. Commit messages are terse and imperative, without personal names. See [AGENTS.md](AGENTS.md).

## License and links

[MIT](LICENSE). Documentation index: [docs/README.md](docs/README.md), [llms.txt](llms.txt). Changelog: [CHANGELOG.md](CHANGELOG.md). CLI reference: [docs/cli.md](docs/cli.md). Troubleshooting: [docs/troubleshooting.md](docs/troubleshooting.md).
