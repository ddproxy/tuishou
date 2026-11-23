# Repository guidance

For contributors and coding agents working in tuishou.

Run checks through the Makefile. The targets are the CI contract.

## Commands

| Command | Effect |
| --- | --- |
| `make help` | List Makefile targets. |
| `make build` | `cargo build --locked`. |
| `make test` | Workflow validation, installer test, then `cargo test --workspace --locked --features test-hooks`. |
| `make installer-test` | Run `tests/installer_test.sh` against the fake release server. |
| `make lint` | rustfmt check, clippy with `-D warnings`, and shellcheck when available. |
| `make docs` | Vale style pass when available. |
| `make docs-check` | Depends on `build`; runs `scripts/docs-check.sh`. That script executes the quickstart commands, validates config examples, and checks links, style and flag drift. |
| `make check` | `lint` plus `docs-check`. |
| `make install` | Run `bash install.sh $(INSTALL_ARGS)`. |
| `make release-check` | Depends on `build`; runs `bash scripts/release-check.sh`. |

## Code map

| Path | Role |
| --- | --- |
| `src/main.rs` | Module wiring, error printing and exit code mapping. |
| `src/cli.rs` | CLI surface: clap flags, subcommands and dispatch. Keep in sync with `docs/cli.md`. |
| `src/component.rs` | Built-in component table: repositories, asset patterns, binaries, config paths and service units. |
| `src/config.rs` | Config keys, defaults and validation. Keep in sync with `docs/configuration.md`. |
| `src/target.rs` | Target detection and target strings. |
| `src/install.rs` | Download, checksum, staged checks, atomic commit and `.prev` rollback. |
| `src/verify/` | In-process Sigstore attestation verification and trust-root handling. No `gh` subprocess. |
| `src/state.rs`, `src/auto_update/state.rs` | Install and auto-update state paths, locking and atomic writes. |
| `src/auto_update/` | One-shot cycle, eligibility, apply paths and timer printer. |
| `src/docs_static.rs`, `build.rs` | Completion and man-page sources, generated under `OUT_DIR`. |
| `install.sh` | Shell bootstrap behavior: installer flags, staging and exit codes. |
| `tests/fixtures/installer/`, `tests/installer_test.sh` | Fake release server, fake binary and installer cases. |
| `scripts/docs-check.sh` | Executes the quickstart commands; keep examples runnable. |
| `docs/README.md` | Documentation index: one topic per page. |

## Rules

- Write terse imperative commit messages. Use no personal names in commits, code comments or docs.
- Make no claims for unimplemented commands. `tuishou top` does not exist; describe it only as planned. The installer labels it planned in its next-steps block; do not automate it.
- Keep docs and CLI in sync: update `src/cli.rs`, `docs/cli.md` and `src/docs_static.rs` together when flags change.
- Keep config docs in sync with `src/config.rs`. Unknown keys are errors, not warnings.
- Use the fake release server fixtures for installer changes and run `make installer-test`. Tests must not depend on live GitHub.
- Do not add `sudo` execution. The installer prints a rerun command for an unwritable prefix; it never invokes `sudo`.
- Preserve pinned workflow action SHAs in `.github/workflows/`. Keep full commit SHAs with version comments.
- Commit no secret-shaped literals. Tokens are read from `GITHUB_TOKEN` or `GH_TOKEN` and redacted from errors.
- Keep test hooks behind the `test-hooks` feature in `src/http.rs` and `src/verify/mod.rs`. Normal release binaries ignore those hooks. `TUISHOU_GITHUB_API_BASE` is honored by normal builds and is not a test-only hook.
