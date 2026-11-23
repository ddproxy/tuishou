# Auto-update

`tuishou` can check GitHub releases, verify the matching artifacts in process,
download them, and apply them without a system service. It is off by default,
and every component must opt in separately.

## Enable it

```toml
[auto_update]
enabled = true
interval = "10m"
policy = "patch"
min_release_age = "10m"
channel = "stable"
require_attestation = true
node_name = "node-1"
reiki_config_path = "/var/lib/reiki/reiki.toml"
agent_unit = "ting.service"
controller_unit = "lu-controller.service"
drain_timeout = "30m"
controller_wait_timeout = "30m"
heartbeat_max_age = "5m"
reiki_health_url = "http://127.0.0.1:8080/health"

# Reiki: staged config check happens before any binary is swapped.
[[component]]
name = "reiki"
auto_update = true
config_path = "/var/lib/reiki/reiki.toml"

# Lu: one release ships both binaries. Nodes run ting, controllers run lu.
[[component]]
name = "lu"
auto_update = true

[verify]
signer_workflow = ".github/workflows/release.yml"
cache_ttl = "24h"
```

`interval` and the other durations use Go-style values such as `10m` or `1h`.
`[auto_update] enabled = true` is not enough: a component is scheduled only when
its own `[[component]]` block sets `auto_update = true`. Listing a name in
`[auto_update].components` filters or orders a cycle but does **not** opt it in.
Defaults are off for all three built-ins. See [components.md](components.md).

`channel` accepts only `stable` (`release` remains a legacy alias); prereleases
are refused. `require_attestation = false` is a configuration error, and
`--allow-unattested` is rejected by auto-update: there is no unverified fallback.
Auto-update honours `min_release_age`, `start_delay_max`, `max_unavailable`
(fleet limit) and bad-version marks on every cycle.

## What happens

1. GitHub's release API is checked for each opted-in component. Release-channel
   rules, version policy, minimum age, held versions and bad-version marks are
   shared with `status` and `update --check`.
2. Each candidate asset is verified against its GitHub attestation in process:
   artifact digest, Sigstore/Fulcio chain, Rekor SET and inclusion proof,
   signing-time validity, and the GitHub OIDC workflow identity and SLSA
   provenance. A TUF refresh may contact `tuf-repo-cdn.sigstore.dev`.
3. A Lu release must contain **both** `lu` and `ting`. A release missing either
   binary is reported as `not in this release`; it is never partially installed.
   Both binaries, their `.prev` backups and any rollback are handled as a pair.
4. The role is detected from installed, enabled systemd units, never from the
   archive: both `systemctl cat <unit>` and `systemctl is-enabled --quiet <unit>`
   must succeed for exactly one of `agent_unit` (`ting.service`) and
   `controller_unit` (`lu-controller.service`). If both or neither qualify, the
   update fails closed.
   - Agent: drain with `lu drain <node>` only when the controller is reachable,
     wait for running jobs up to `drain_timeout`, stage/swap binaries, restart
     `ting.service`, check `lu nodes --json` for the new version and a heartbeat
     no older than `heartbeat_max_age`, then `lu undrain <node>`. Undrain is
     attempted on every later failure path, including a failed rollback.
   - Controller: require `controller_policy` of `when_idle` or `window` (the
     default `manual` skips), take a backup with `lu backup --out ...` and refuse
     the update if it fails or is empty, wait until `lu status --json` reports no
     leased or running jobs (up to `controller_wait_timeout`), then swap both
     binaries, restart `lu-controller.service` and confirm the new version.
   - Reiki: run the new binary's `reiki check --config <path>` **before** any
     binary is swapped. A failure keeps the old binary and marks the candidate
     bad. After restart, require the unit to be active, `reiki check` to pass,
     and the `reiki_health_url` admin endpoint to be healthy.
5. On any failed install, restart or health check, both binaries in the pair are
   restored from `.prev`, the unit is restarted and the old version is checked
   again; the new version is marked bad and recorded in status. Reiki is applied
   before Lu even when the config lists them the other way around.
6. Every component gets exactly one status line from the fixed vocabulary in
   [components.md](components.md). `--json` reports the same data.

Verification failures never hide other components: a cycle reports all opted-in
components and exits 1 when any of them failed. Journal events are fixed,
one-line and token-free (for example `agent_drain`, `controller_backup`,
`reiki_health_failed`).

## State and receipts

By default, state is under `~/.local/state/tuishou` (or `$XDG_STATE_HOME/tuishou`).
Use `tuishou auto-update status --json` or `tuishou status --json` to inspect
per-component versions, `last_check`, `last_result` and `bad_versions`.
`tuishou status` reports `installed_version` and `latest_version`;
`auto-update status` reports `installed_version`, `last_version` and
`next_eligible_version`. With `--prefix`, state lives under `<prefix>/.tuishou`.

The verifier cache defaults to a `0700` directory under the state directory
unless `[verify].cache_dir` is set.

## Scheduling

Shipped units `contrib/tuishou-update.service` and `contrib/tuishou-update.timer`
run one cycle with `RandomizedDelaySec` and `Persistent=true`. Adjust the
`ExecStart` config path and interval to match your deployment, then enable the
timer. `tuishou auto-update install-timer --interval 10m` prints an equivalent
pair (it never installs anything). To enable a single component, set its
`[[component]] auto_update = true` and restart the timer; other components stay
off. See [node-update.md](node-update.md) for the single-node procedure and fleet
settings.
