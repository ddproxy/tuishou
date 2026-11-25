# Second-round security review notes

Scope: findings SR2-1 through SR2-14 from the read-only second-round review.

Every finding was checked against the current source before work began. All 14
were confirmed as real and fixed. No finding was rejected.

## Findings and fixes

- **SR2-1 (High) — archive-supplied symlink executed before attestation.** The
  installer now lists archives with `tar -tvzf`, accepts exactly one regular
  `tuishou` entry, rejects links, devices, absolute names, parent components and
  duplicates, extracts only into a private staging directory, and verifies the
  staged binary before touching the destination. Installer cases cover symlink,
  absolute-name, parent-path and duplicate archives.

- **SR2-2 (High) — `top` restored unvalidated argv from shared state.** TUI state
  is now scoped per config path and prefix, opened without following symlinks,
  owner/mode checked from the descriptor, and read through that descriptor.
  Saved argv is ignored entirely. Child programs go through the safe resolver
  and executable vetting. Tests cover config scoping and unsafe state files.

- **SR2-3 (Medium) — writable prefixes and pre-verification execution of an
  existing destination.** Prefix path components, prefix and binary directory
  ownership and mode are checked before use and re-checked after directory
  creation. The existing version is read from local state, never by executing
  the destination. The destination is only replaced after the staged binary
  passes verification. Unwritable, world-writable, foreign-owned and symlinked
  prefixes are rejected; a test proves an existing destination is never run.

- **SR2-4 (Medium) — unbounded installer downloads and extraction.** Metadata,
  archive and checksum downloads have fixed byte caps. Archive listings are
  bounded by entry count and line length, entry size is bounded, and extraction
  writes into private staging. Installer cases cover an oversized checksum asset
  and an oversized archive listing.

- **SR2-5 (Medium) — config trust check followed symlinks and raced.** The
  config is opened once with `O_NOFOLLOW`, owner and mode are checked on the
  opened descriptor, and parsing reads from that same descriptor. Symlink and
  group-writable config cases are covered.

- **SR2-6 (Medium) — health URL loopback bypass.** Health URLs are parsed into
  components, userinfo is rejected, and plain HTTP is allowed only for exact
  loopback hosts. All other hosts require HTTPS and the normal host allowlist.

- **SR2-7 (Medium) — timer executable path bypass.** `..` components are
  rejected before canonicalization. `TUISHOU_TIMER_EXE` overrides are retained
  for operator use but must canonicalize inside a trusted directory to an
  existing regular file with acceptable owner and mode. The hardcoded default
  need not exist on the host generating the unit.

- **SR2-8 (Medium) — child environment inheritance in `top`.** PTY children are
  started with a cleared environment plus a small allowlist. Updater tokens are
  not inherited.

- **SR2-9 (Medium) — terminal escape injection through `top` exit hints.**
  Child-derived text is sanitized for C0/C1 controls before it reaches the host
  footer, and `logging::one_line()` applies the same sanitization.

- **SR2-10 (Low) — shipped systemd unit omitted PATH pin.** The generated unit
  places `Environment=PATH=...` after `EnvironmentFile=`, the shipped contrib
  unit now carries the same pin, and mutable environment files are guarded.

- **SR2-11 (Low) — expired HOLD removal without the cycle lock.** `active_hold()`
  is read-only. Expiry removal moved to `expire_hold()`, which is called only
  from locked mutating paths.

- **SR2-12 (Low) — only one token value redacted.** Redaction covers both
  `GITHUB_TOKEN` and `GH_TOKEN` plus token-shaped values independent of the
  environment. Tests build shaped values at runtime instead of storing literal
  secret-shaped text.

- **SR2-13 (Low) — uncapped non-2xx error bodies.** Error bodies now use the
  same bounded read path as successful responses before being flattened for
  display.

- **SR2-14 (Low) — docs and CI denied the shipped `top` feature.** CLI, limits,
  security and top documentation now describe the shipped feature, its flags,
  config-scoped state and child environment handling. The docs drift check now
  requires accurate `top` coverage instead of requiring it to be absent.

## Validation

`make check` and `make test` pass: rustfmt, clippy, docs checks, workflow
validation, 24 installer cases and 226 Rust tests.

## Residual risks

- The optional `--expect-sha256` pin remains the only true bootstrap root of
  trust; a checksum from the same release is consistency, not attestation.
- In-process attestation cannot verify the verifier itself, and there is no
  offline installer attestation.
- `TUISHOU_TIMER_EXE` remains an operator override, now strictly validated, so
  a deliberately privileged operator can still choose a trusted executable.
- `top` persists the active tab and restart counts, but no executable argv.

## Dev tag identity claim check

The verifier requires signer identity `release.yml@refs/tags/<tag>`. It compares the full SAN URI
`https://github.com/<owner>/<repo>/.github/workflows/release.yml@refs/tags/<tag>`
byte for byte (`src/verify/sigstore_backend.rs`), with the default workflow path
set in `src/config.rs`. Docs and tests follow the full URI from the code.

The dev publishing claim matches the workflows: `.github/workflows/dev-release.yml`
pushes `refs/tags/$TAG` and runs `gh workflow run release.yml --ref "$TAG"`;
`.github/workflows/release.yml` enables `workflow_dispatch` and builds only when
`github.ref_type == 'tag'`. `scripts/set-release-version.py` formats dev tags as
`vX.Y.(Z+1)-dev.N.g<hash>`, which covers the example tag in the task.

`tuishou verify` requires `--repo` and `--tag`. `AttestationRequest.tag` is
optional internally, but the install and verify paths always set it. No code path
accepts a `refs/heads/...` workflow identity.

## Validation note

`cargo test --workspace` without features fails in `tests/auto_update.rs`: those
tests use an HTTP fake server, and normal builds correctly refuse insecure
loopback URLs. The Makefile/CI contract is
`cargo test --workspace --locked --features test-hooks`, which passes along with
`cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -D warnings`
and `scripts/docs-check.sh`.

## Self-update claim validation

Each claim below was checked against the current source; no contradictory code path
was found.

- Opt-in: `AutoUpdateConfig.self_update` is field `self` with `default` and the
  struct denies unknown fields (`src/config.rs`). Default is false, and
  `self_update = true` is refused. Cycle execution also requires
  `[auto_update] enabled = true` (`src/auto_update.rs`).

- Component separation: the built-in `tuishou` entry keeps `auto_update = false`
  (`src/component.rs`). `cycle_names` ranks it last, and the generic disabled
  check exempts it only when `self_update` is set. The apply branch then calls
  `self_apply::apply_self` only for that name (`src/auto_update.rs`).

- Verification: `apply::install_options` sets `allow_unattested = false` and passes
  the loaded verify config; `install_component` builds `AttestationRequest` with
  `workflow_for("tuishou")`, the release tag and source digest (`src/install.rs`).
  Stable and dev tags therefore use the same workflow identity check described in
  the earlier dev tag note.

- Atomic replacement: staged commit writes temp files beside the destination,
  calls `sync_all()` before `NamedTempFile::persist`, then best-effort fsyncs the
  parent directory (`src/install.rs`). `self_apply` sets the install prefix to the
  running binary's directory and records the previous version before applying.

- Timer safety: the only restart command is
  `systemctl try-restart tuishou-auto-update.timer` (`src/auto_update/self_apply.rs`).
  The oneshot `tuishou-auto-update.service` is not restarted, and a restart
  failure is logged without rolling back.

- Confirmation and rollback: `self_pending` holds path, version and previous
  version in `auto.json` (`src/auto_update/state.rs`). The first scheduled pass
  confirms `<binary> --version` before fleet and hold gates. A staged smoke
  failure keeps the old binary and marks the tag bad; a first-pass start or tag
  mismatch restores `.prev`, marks the tag bad and fails that cycle
  (`src/auto_update/self_apply.rs`).

- Test coverage: `tests/auto_update.rs` uses fake releases for the disabled
  default, stable selection, dev selection, attestation failure, staged smoke
  failure and first-pass rollback. `src/auto_update/self_apply.rs` adds runner
  tests for timer-only restart, version matching and rollback from `.prev`.

## Self-update validation run

`cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets --features test-hooks -- -D warnings`,
`cargo test --workspace --features test-hooks` and `bash scripts/docs-check.sh`
pass. The docs check runs the quickstart commands, validates 15 config examples,
resolves local links and passes style, flag-drift and literal checks.

`make check` and `make test` also pass: rustfmt, clippy, docs checks, workflow
validation, 24 installer cases and 249 Rust tests (196 unit, 16 auto-update
integration, 2 docs, 21 integration, 5 self-update, 4 top and 5 validator).
Shellcheck is not installed in this environment and was skipped. Cargo reports
an unrelated warning that the placeholder `fw-tui` patch is unused.

## Public repository hygiene review

Every candidate below was checked against the current code and docs. No
behavior, wire format or stored value was changed.

- Internal-looking node names appeared only in auto-update unit fixtures.
  They were renamed to a neutral name; no release archive, state key or CLI
  value used them.
- Internal-looking labels in health-URL tests were renamed to reserved neutral
  labels. The tests still cover HTTPS acceptance, cleartext refusal, userinfo
  rejection and loopback lookalikes.
- An internal workspace path in attestation fixture instructions was replaced
  with repository-relative destinations.

Verified and left unchanged:

- Signed upstream trust-root metadata carries upstream key-owner labels and a
  cloud KMS URI inside its signed body. `src/verify/trust.rs` embeds the file
  and pins its checksum, so editing it would break trust-root validation. The
  exception is path-scoped in `tests/fixtures/repo-hygiene-allowlist.txt` with
  a reason; no label value is repeated here.
- Signed upstream attestation fixtures carry upstream certificate and workflow
  identities, including an RFC822 subject alternative name. They are pinned by
  checksum in `tests/fixtures/attestation/README.md` and used only for negative
  tests or as a historical positive release bundle.
- The MIT license notice is public attribution.
- Loopback addresses, RFC 5737 documentation addresses, reserved `.example`,
  `.test` and `.invalid` labels and public GitHub or Sigstore endpoints are
  legitimate. No RFC 1918, CGNAT or link-local address was found.
- Workflow and job names are public generic CI identifiers. No internal job
  name was found.

`tests/repo_hygiene.rs` fails when a repository file reintroduces a forbidden
identifier outside the allowlist. `make lint`, `make test` and `make docs-check`
pass with the new check enabled.
