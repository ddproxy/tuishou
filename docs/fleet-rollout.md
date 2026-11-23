# Fleet rollout

For operators enabling unattended updates on a fleet of agent nodes. Read [node-update.md](node-update.md) for the single-node procedure first.

Nothing pushes updates: each node pulls, verifies, drains, installs, restarts, checks health and rolls back locally. Keep the fleet binary-only, arm one canary, and widen only when that canary's journal and status agree.

## Canary pattern

One canary node runs the timer with its components opted in. Every other node installs the same verified binaries but keeps `[auto_update] enabled = false` and leaves `[[component]] auto_update` at its default `false`. Binary-only nodes still receive updates through the manual `tuishou update` path; they never start a scheduled cycle.

Canary config, with the node name taken from the controller's node list:

```toml
[auto_update]
enabled = true
interval = "10m"
policy = "patch"
min_release_age = "10m"
channel = "stable"
require_attestation = true
node_name = "node-1"
components = ["lu", "reiki"]

[[component]]
name = "lu"
auto_update = true

[[component]]
name = "reiki"
auto_update = true
```

Fleet config:

```toml
[auto_update]
enabled = false
```

`[auto_update] enabled = true` alone schedules nothing; each component needs its own `auto_update = true`. `min_release_age` keeps a release out of the canary until its `published_at` is at least that old; a release with no usable publication time is never eligible while the age delay is set. Keep `interval` at `10m` or above and `policy = "patch"` for the canary. `require_attestation = false` and `--allow-unattested` are refused by auto-update.

`tuishou auto-update install-timer --interval 10m` prints a unit pair but does not install or enable it. Install the shipped `contrib/tuishou-update.service` and `contrib/tuishou-update.timer`, or the printed pair. Stage the canary behind a hold, arm the timer, inspect status, then release the hold:

```sh
tuishou --config /etc/tuishou.toml auto-update hold --reason "canary staging"
systemctl enable --now tuishou-update.timer
tuishou --config /etc/tuishou.toml auto-update status --json
tuishou --config /etc/tuishou.toml auto-update release
```

After one release has been applied and survived a full canary soak, copy the canary config to the next nodes with their own `node_name` values and enable their timers a few at a time. `[update] start_delay_max` spreads starts inside a cycle; `[update] max_unavailable` skips a cycle when the controller already reports enough draining or offline nodes, and fails closed when the controller state cannot be read.

## Hold, release and status

These controls are local: run them on the node with the same `--config` (or `--prefix`) as the timer. There is no remote hold or remote roll command.

| Command | Effect |
| --- | --- |
| `tuishou auto-update status --json` | Reports `enabled`, `last_check`, `interval`, `policy`, `controller_policy`, `hold` and per-component rows with `status`, `last_result`, `installed_version`, `last_version`, `next_eligible_version` and `bad_versions`. |
| `tuishou auto-update hold --reason <text>` | Creates `HOLD`. `--expires-at <RFC3339>` or `--expires-in <duration>` adds an expiry; the two are mutually exclusive. |
| `tuishou auto-update release` | Removes `HOLD`. Removing an absent hold is not an error. |
| `tuishou auto-update clear-bad [<component>]` | Forgets bad-version marks and failure counts for one component or all components. Use only after the cause is fixed. |

A hold pauses every component in the cycle and exits `0`; it does not block manual `tuishou update`. Holds, results and bad-version marks live in `<prefix>/.tuishou/` (`auto.json`, `HOLD`, `lock`), otherwise under `$XDG_STATE_HOME/tuishou` or `~/.local/state/tuishou`. Expired holds are removed when read. The plain `status` view prints `hold=active|none` and a `bad=<versions>` column.

## Node name and localhost

The apply path resolves the node name in this order: `[auto_update].node_name`, then `TUISHOU_NODE_NAME`, then `HOSTNAME`, then the literal `localhost`. Only non-empty values are accepted. Set `node_name` explicitly on every node that arms a timer: drain, health and journal lines are keyed by this name, and the controller must know the same name. `localhost` is a last-resort sentinel, not a fleet identity. This build has no built-in refusal of localhost; a node reaching the fallback drains by that name and fails closed when the controller does not know it. Refuse to enable the timer in your own rollout check until the name resolves to a controller-known node.

## Drain and node credentials

`ting` is the agent binary shipped in the `lu` release and `ting.service` is the default agent unit. Commands such as `ting drain`, `ting wait-idle`, `ting status --json` and `ting undrain` do not exist in this build; do not put them in runbooks. Agent-side drain runs through the operator CLI on the node: `lu status --json` checks controller reachability, `lu drain <node>` stops new work, `lu nodes --json` is polled until the node reports `draining: true` with no `current_job`, and `lu undrain <node>` releases it. `ting.service` is restarted between install and health check. Every child runs with a cleared environment, an argument vector and a `command_timeout` (default `30s`); no token is injected into it. Binary-only nodes need no operator credential because they never run this path. There is no supported "no operator credential on nodes" mode for timer-enabled nodes in this build: they need local access to the Lu control plane. Keep any operator credential off shared images and out of the canary config.

The cycle journals its drain phases as `agent_drain`, `agent_drain_pending`, `agent_drained` and `agent_undrain`. `agent_update_start` carries `drain=true|false`; false means no controller confirmation was obtained, so no drain was performed. The apply still installs and restarts the agent, and the `lu nodes --json` health check then fails closed. A drain that times out is undrained before any binary is replaced. The pre-commit drain recheck is part of the same transaction as the binary swap and state commit.

## Rollback and bad-version marking

A Lu release carries both `lu` and `ting`; the pair is staged, backed up as `.prev`, restarted and rolled back together. The role comes from installed and enabled systemd units, never from archive contents: both or neither of `ting.service` and `lu-controller.service` qualifying fails closed. Reiki is applied before the Lu agent when both are due.

If the restart, health check, drain recheck or state commit fails, the cycle restores `.prev`, restarts the unit, verifies the old version is healthy, marks the attempted version bad in `auto.json` and undrains the node. The bad mark is written before recovery starts, so a retry cannot miss it. A bad version is skipped on later cycles with `reason=<tag> is marked bad` until `auto-update clear-bad` is run. Agent paths attempt undrain after failed recovery; an unverified Reiki recovery leaves the node drained for operator action. Both report `recovery failed: ...` in `auto_update_failed`. `tuishou rollback <component>` restores the current `.prev` by hand; a missing `.prev` is an error, not a silent success.

## Reading the journal

Structured lines go to stderr as `tuishou: event=<name> key=value ...` and are captured by systemd. One cycle produces a fixed, token-free vocabulary; use it for canary comparison instead of free-text matching.

```sh
journalctl -u tuishou-update.service -o cat | grep 'tuishou: event='
```

Use the printed unit name instead of `tuishou-update.service` when you installed the `install-timer` units.

Cycle events: `auto_update_cycle_start`, `auto_update_fleet_skipped`, `auto_update_held`, `component_auto_update_disabled`, `release_list_failed`, `release_list_cached`, `eligibility_failed`, `component_skipped`, `auto_update_applied`, `auto_update_skipped`, `auto_update_failed`, `auto_update_cycle_done`.

Agent events: `agent_skipped`, `agent_update_start`, `agent_drain`, `agent_drain_pending`, `agent_drain_poll_failed`, `agent_drained`, `agent_installed`, `service_restart`, `agent_commit_recheck_failed`, `agent_update_ok`, `agent_health_failed`, `agent_undrain`.

Other events: `hold_set`, `hold_released`, `hold_absent`, `hold_expired`, `bad_cleared`, `auto_update_lock_busy`, `notify_failed`, `component_error`, `lu_role_failed`, `lu_role_detected`, `tool`, `rate_limited`, `trust_root_fallback`, `reiki_config_check`, `reiki_staged_check_failed`, `reiki_health_endpoint`, `reiki_health_failed`, `controller_backup`, `controller_skipped`, `controller_wait_busy`, `controller_idle`, `controller_idle_rechecked`, `controller_health_failed`.

A healthy canary cycle ends with `event=auto_update_cycle_done failed=false` and, for the applied component, `event=auto_update_applied component=lu version=<tag>`. Any `failed=true` cycle stops the rollout.

## Troubleshooting

Rows are keyed by messages the code emits; `<...>` marks a value taken from the node, config or release.

| Journal message | Meaning | Action |
| --- | --- | --- |
| `event=auto_update_held reason=<reason>` | The `HOLD` file is active; no component was checked. | Inspect `auto-update status`, then run `auto-update release` when the canary may proceed. |
| `event=auto_update_fleet_skipped reason=fleet limit (N of M nodes unavailable)` | `[update] max_unavailable` is already met. | Wait for other nodes to finish; lower the limit only with an availability review. |
| `event=auto_update_fleet_skipped reason=fleet limit (controller state unavailable: ...)` | The controller node list could not be read. | Restore controller access; the cycle fails closed and retries on the next timer tick. |
| `event=component_auto_update_disabled component=<name>` | The component is in `components` but has no `[[component]] auto_update = true`. | Opt the component in on the canary, or leave the node binary-only. |
| `event=release_list_failed component=<name> reason=GET <url> failed with status <status>` | Release metadata could not be fetched. | Check API access and rate limits; `event=rate_limited url=<url> retry_after=<seconds>` precedes the error `rate limited by <url>; retry after <delay>s`. |
| `event=component_skipped component=<name> reason=<tag> is younger than min_release_age <duration>` | The candidate is too new. | Wait for the age delay; check `published_at` with `auto-update status`. |
| `event=component_skipped component=<name> reason=<tag> has no published_at; cannot satisfy min_release_age <duration>` | Age cannot be established. | Wait for usable release metadata; do not shorten the age check for the canary. |
| `event=component_skipped component=<name> reason=<tag> is marked bad` | A previous attempt marked the version bad. | Find the earlier `agent_health_failed` or `auto_update_failed`, fix the cause, then run `auto-update clear-bad <name>`. |
| `event=lu_role_failed component=lu reason=both <agent_unit> and <controller_unit> are installed and enabled; refusing to guess the Lu role` | Both units qualify. | Disable the wrong unit; role detection never guesses. |
| `event=lu_role_failed component=lu reason=neither <agent_unit> nor <controller_unit> is installed and enabled; refusing to guess the Lu role` | No unit qualifies. | Install and enable the intended unit before arming the timer. |
| `event=agent_drain_poll_failed reason=<error>` | Drain state could not be read from the controller. | Restore controller access; a drain timeout is undrained before any swap. |
| `event=agent_commit_recheck_failed node=<name> reason=<reason>` | Drain state was lost before the state commit. | The cycle rolls back; check the controller's node list before resuming the rollout. |
| `event=agent_health_failed component=lu version=<tag> node=<name> reason=node <name> did not report version <tag> with a fresh heartbeat within <duration>` | The restarted agent did not pass health. | Read the following `auto_update_failed` line for rollback and bad-mark details; check the unit and heartbeat. |
| `event=auto_update_failed component=<name> reason=health check failed within <duration> for <component> on <node>: ...; marked <tag> bad; recovery failed: ...` | Rollback did not finish. | Check `.prev`, the failed recovery step and whether the node was left drained before retrying. |
| `event=auto_update_lock_busy` | A `run` found another cycle holding the lock and did no work. | Wait for the running command; do not remove the `lock` file by hand. |
| `event=hold_absent` | `auto-update release` found no hold. | Confirm the resolved prefix matches the timer's config. |
| `event=bad_cleared component=<name>` | Bad marks and failure counts were cleared. | Re-arm one canary first; do not clear the fleet at once. |

Related reading: [auto-update.md](auto-update.md), [configuration.md](configuration.md), [troubleshooting.md](troubleshooting.md) and [cli.md](cli.md).
