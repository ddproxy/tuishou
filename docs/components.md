# Components

`tuishou` ships with three built-in components. This table is the single source
of truth for their release artifacts, services and health checks; `tuishou.toml`
may override paths and policy for these names, but never the repository or asset
host. Downloads come only from GitHub release assets.

| Component | Repository | Release asset (plus `.sha256`) | Required binaries | Service unit | Signer workflow | Health check |
| --- | --- | --- | --- | --- | --- | --- |
| `reiki` | `ddproxy/reiki` | `reiki-<tag>-<target>.tar.gz` | `reiki` | `reiki.service` | `.github/workflows/release.yml` | The new binary passes `reiki check --config <path>` **before** the swap; after restart the unit is active, `reiki check` still passes, and `reiki_health_url` (default `http://127.0.0.1:8080/health`) is healthy. |
| `lu` | `ddproxy/tuishou-lu` | `lu-<tag>-<target>.tar.gz` | `lu` **and** `ting` | `ting.service` (agent) or `lu-controller.service`; chosen from installed, enabled units | `.github/workflows/release.yml` | Agent: `lu nodes --json` shows the exact new version with a heartbeat within `heartbeat_max_age`. Controller: `lu status --json` shows the new version after a successful `lu backup` and an idle-job wait. |
| `tuishou` | `ddproxy/tuishou` | `tuishou-<tag>-<target>.tar.gz` | `tuishou` | none | `.github/workflows/release.yml` | The installed binary reports the expected version. |

A Lu archive missing either `lu` or `ting` is not a complete release: it is
reported as `not in this release (archive lacks: ...)` and never partially
installed. Both binaries are swapped and rolled back together.

Role detection never inspects archive contents. A unit qualifies only when both
`systemctl cat <unit>` and `systemctl is-enabled --quiet <unit>` succeed. Exactly
one of `agent_unit` and `controller_unit` must qualify; ambiguous or missing
roles fail closed. The defaults are `ting.service` and
`lu-controller.service`.

Every command reports components in the order above. A normal line uses one fixed
phrase: `up to date (vX)`, `update available vA -> vB`, `updated vA -> vB`,
`not installed (latest vB)`, `not in this release (archive lacks: <binaries>)`,
`held (reason)`, `bad version skipped (vX)`, `skipped (reason)` or `failed (reason)`.
`--json` reports the same data. Both status commands additionally carry
`auto_update`, `last_check`, `last_result` and `bad_versions` per component:
`tuishou status --json` uses `installed_version` and `latest_version`, while
`tuishou auto-update status --json` uses `installed_version`, `last_version` and
`next_eligible_version`.

Automatic updates are per-component opt-in: set `auto_update = true` in the
`[[component]]` block for each component you want scheduled. Listing names in
`[auto_update].components` does not opt them in.
