# Fuzzing

For contributors: exercise parsers of untrusted and semi-trusted input with cargo-fuzz (libFuzzer).

This crate is a separate workspace. `cargo test --workspace` in the repository root never
builds these targets. The code under test is the library in `src/lib.rs`, compiled without
the `top` feature so no TUI or PTY code enters a fuzz run.

## Targets

| Target | Input under test | Asserted invariant |
| --- | --- | --- |
| `config_toml` | `tuishou.toml` bytes decoded as UTF-8 and parsed by `toml` | Parsing and validation are deterministic; validation success implies `require_attestation` is true. |
| `release_json` | GitHub release API JSON decoded by `serde_json` | A decoded release survives a serialization round trip, matches `parse_releases`, and binds to its canonical tag. |
| `node_json` | `lu nodes --json` and `lu status --json` output | The unavailable count never exceeds the decoded node count; drained nodes report `draining: true` with no current job; health success implies the expected version and an online node. |
| `version_tag` | release tags and version strings | `format_version` round-trips through `Version::parse`; a version never outranks itself or is eligible onto itself under any policy. |

All targets return early when the input is not valid UTF-8 or not valid TOML/JSON. They never
spawn processes or read credentials. The config target calls the real
`AutoUpdateConfig::validate`, which probes an IP-literal health URL by binding an ephemeral
local listener; no outbound connection is made.

## Seeds

`corpus/<target>/` holds small seeds derived from existing test data: the configuration
examples in `docs/configuration.md`, the release JSON shape in
`tests/fixtures/installer/fake_release_server.sh`, the node and status JSON shapes in
`src/lu.rs` and `tests/auto_update.rs`, and the stable, pre-release and dev tag examples in
the documentation. URLs in seeds point only to loopback. No seed contains a token or other
secret-shaped literal.

## Run locally

Install the tools once:

```sh
rustup toolchain install nightly --profile minimal
cargo install cargo-fuzz --locked
```

Run one target indefinitely, stopping with Ctrl-C:

```sh
cargo +nightly fuzz run release_json
```

Run every target in a short, CI-friendly pass, writing crash artifacts to
`fuzz/artifacts/<target>/`:

```sh
make fuzz-smoke
```

Set `FUZZ_SECONDS` to change the per-target budget and `FUZZ_TOOLCHAIN` to override
`+nightly`. LibFuzzer writes crashing inputs to the artifact prefix; keep those files out
of the repository and open a bug with the reproducer instead.

When cargo-fuzz is not installed, type-check the crate instead:

```sh
cargo check --manifest-path fuzz/Cargo.toml
```

The weekly `Fuzz` workflow runs `make fuzz-smoke` and uploads crash artifacts only when a
run fails.
