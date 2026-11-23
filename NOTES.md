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
