# Components

`tuishou` ships with three built-in components. This table is the single source of
truth for their release artifacts, services and health checks; `tuishou.toml` may
override paths and policy for these names, but never the repository or asset host.
Downloads come only from GitHub release assets.

| Component | Repository | Release asset (plus `.sha256`) | Binaries | Service unit | Signer workflow | Health check |
| --- | --- | --- | --- | --- | --- | --- |
| `reiki` | `ddproxy/reiki` | `reiki-<tag>-<target>.tar.gz` | `reiki` | `reiki.service` | `.github/workflows/release.yml` | After install, `reiki check --config <config path>` must pass before the unit is restarted; after restart the unit must be active and `reiki check` must still pass. The config path defaults to `/var/lib/reiki/reiki.toml` and can be overridden. |
| `lu` | `ddproxy/tuishou-lu` | `lu-<tag>-<target>.tar.gz` | `lu`, plus `lu-agent` or `ting` when the archive holds them | `lu-agent.service` when present | `.github/workflows/release.yml` | The unit is active and the node reports the exact installed version after restart. The node agent lives inside the `lu` binary until a release ships a separate binary. |
| `tuishou` | `ddproxy/tuishou` | `tuishou-<tag>-<target>.tar.gz` | `tuishou` | none | `.github/workflows/release.yml` | The installed binary reports the expected version. |

`ting` is not a default until a release archive contains a `ting` binary. The code
that recognises it remains in place so it can return as a one-line change.

Every command reports components in the order above. A normal line uses one fixed
phrase: `up to date (vX)`, `update available vA -> vB`, `updated vA -> vB`,
`not installed (latest vB)`, `not in this release (archive lacks: <binaries>)`,
`held (reason)`, `bad version skipped (vX)`, `skipped (reason)` or `failed (reason)`.
`--json` reports the same data.
