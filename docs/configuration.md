# Configuration

For operators and agents editing `tuishou.toml`.

The default path is `./tuishou.toml`. `--config <path>` or `TUISHOU_CONFIG` overrides it. The file must be owned by the running user or root and must not be writable by group or others. Unknown keys are rejected. CLI `--prefix` wins over `auto_update.prefix`.

## Keys

| Section and key | Default | Meaning |
| --- | --- | --- |
| `[[component]] name` | built-ins only | Override binaries, ranges, units or auto-update for a built-in component. |
| `[[component]] binaries` | per component | Expected archive members; `lu` requires `lu` and `ting`. |
| `[[component]] auto_update` | `false` | Per-component opt-in for scheduled updates. |
| `[auto_update] enabled` | `false` | Master switch; no scheduled update runs while false. |
| `prefix` | unset | Install root for binaries, state and verify cache. |
| `interval` | `10m` | Timer interval; minimum `5m`. |
| `components` | `reiki`, `lu`, `tuishou` | Components auto-update considers. |
| `policy` | `patch` | `patch`, `minor` or `any`. |
| `min_release_age` | `10m` | Delay before a release is eligible. |
| `channel` | `stable` | Only stable releases; `release` is a legacy alias. |
| `max_version` | unset | Upper version bound. |
| `require_attestation` | `true` | Always true; `false` is a config error. |
| `controller_policy` | `manual` | `manual`, `when_idle` or `window`; non-manual needs `backup_dir`. |
| `backup_dir`, `backup_keep` | unset, `5` | Controller backup directory and retention. |
| `window_start`, `window_end` | unset | Required together for `window` policy. |
| `drain_timeout` | `30m` | Maximum drain wait. |
| `controller_wait_timeout` | `30m` | Maximum wait for an idle controller. |
| `health_timeout` | `5m` | Maximum health-check wait. |
| `heartbeat_max_age` | `5m` | Node heartbeat freshness. |
| `command_timeout` | `30s` | Deadline for `lu` and `systemctl` children. |
| `node_name` | unset | Node identity; falls back to `TUISHOU_NODE_NAME` then `HOSTNAME`. |
| `agent_unit`, `controller_unit` | `ting.service`, `lu-controller.service` | Units used by apply and health paths. |
| `reiki_config_path` | unset | Overrides the Reiki config file checked around a restart. |
| `reiki_health_url` | `http://127.0.0.1:8080/health` | Reiki health endpoint. |
| `notify_command` | empty | Argument vector run with a JSON summary; no shell. |
| `notify_after_failures` | `3` | Consecutive failures before notification. |
| `[update] start_delay_max` | `0` | Random delay before a cycle. |
| `max_unavailable` | unset | Skip a cycle when this many nodes are unavailable. |
| `[verify] signer_workflow` | `.github/workflows/release.yml` | Expected Fulcio SAN workflow path. |
| `verify.components.<name>.signer_workflow` | unset | Per-component signer override. |
| `verify.trust_root` | `embedded` | Pinned root with TUF refresh, or an operator-supplied path. |
| `verify.cache_dir` | state parent | Trusted-root cache directory. |
| `verify.cache_ttl` | `24h` | Cache lifetime before TUF refresh. |

`repo` and `asset_pattern` cannot be overridden: downloads come only from built-in GitHub release assets.

## Minimal example

```toml
[auto_update]
enabled = true
```

## Full example

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

Validate an example with `tuishou --config <file> auto-update install-timer`. That command prints systemd units and exits 0 without installing them; `scripts/docs-check.sh` runs it against both examples.
