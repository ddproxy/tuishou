# Scheduled auto-update (opt-in)

> **Not recommended for production.** Auto-update installs new releases without human review. It exists for fleets that update very often and accept that risk. It is off by default and every safeguard below is designed to make accidental enablement difficult.

## What it does

`tuishou auto-update` runs one update cycle at a time. The scheduler is external: a systemd timer or cron invokes `tuishou auto-update run --once`. There is no long-running daemon.

For each configured component the cycle:

1. Takes an exclusive `flock` on `<state dir>/lock`; a second concurrent run exits quietly.
2. Lists GitHub releases with conditional requests (`ETag`) and honours rate limits / `Retry-After`. `GITHUB_TOKEN`/`GH_TOKEN` is used only when set.
3. Selects the newest eligible release according to policy, channel, `min_release_age`, `max_version`, bad-version marks and hold state.
4. Applies the same verified install path as manual updates: checksum, GitHub attestation, safe extraction, atomic replace, `.prev`, and optional `--version` smoke check.
5. Writes single-line journal events and a state file for `tuishou auto-update status`.

## Enabling (opt-in only)

Auto-update never runs unless the config explicitly enables it:

```toml
[auto_update]
enabled = true                 # default false; required
interval = "10m"               # 5m..15m recommended, below 5m rejected
components = ["lu", "ting"]
policy = "patch"               # patch | minor | any
min_release_age = "10m"        # release must be this old before install
channel = "release"            # release | prerelease
require_attestation = true     # cannot be false in auto mode
attestation_signer = "ddproxy/tuishou-lu/.github/workflows/release.yml"
# max_version = "v0.5.0"
# notify_command = ["/usr/local/bin/notify", "tuishou"]
# notify_after_failures = 3
```

Unknown keys in `tuishou.toml` are errors. `--allow-unattested` is refused in auto mode and `gh` must be on `PATH`.

## Policy and 0.y versions

- `patch`: same major and minor only.
- `minor`: same major, but for `0.y.z` a minor bump is treated as a breaking change and is refused (same as `patch`).
- `any`: allows any newer version, including breaking changes.

`min_release_age` gives operators time to pull a bad release before fleets install it. A release is eligible only after it has existed for at least that duration.

## Hold and bad versions

```sh
tuishou auto-update hold --reason "incident" --expires-in 2h
tuishou auto-update release
tuishou auto-update clear-bad            # all components
tuishou auto-update clear-bad ting       # one component
```

When `<state dir>/HOLD` is present, all updates pause. The hold may include a reason and expiry; an expired hold is removed on sight. A version that fails its health check is marked bad in state and never retried until a newer version appears or `clear-bad` is run.

## Node agent updates

For `ting` or a component that names the current agent binary, auto-update uses the drain path from `roll`:

1. `lu node drain <name>`
2. wait for running jobs up to `drain_timeout`
3. verified install
4. `systemctl restart <unit>`
5. wait for unit active and `lu node <name> --json` to report the new version within `health_timeout`
6. `lu node undrain <name>`

If the node does not become healthy, tuishou restores `.prev`, restarts the unit, undrains, marks the version bad, and exits non-zero.

## Controller updates

Controller updates are conservative:

- `controller_policy = "manual"` (default): never auto-update the controller.
- `when_idle`: update only when `lu job list --json` reports no leased/running jobs. Takes `lu backup --dir <backup_dir>` before install and keeps the last `backup_keep` backups.
- `window`: like `when_idle`, but only inside the configured UTC window.

Schema changes are never crossed automatically. If the release notes contain the line `Requires: new controller database`, or the change is a 0.y minor under `patch`, auto-update skips with a clear logged reason. The Lu release notes will include that marker when true.

## systemd timer

`tuishou auto-update install-timer` prints a service and timer unit pair; it does not install them. The timer uses `OnUnitActiveSec` (default 10 minutes, configurable), `RandomizedDelaySec` jitter, and `Persistent=true`.

```sh
tuishou auto-update install-timer --interval 10m | sudo tee /etc/systemd/system/tuishou-auto-update.units
# Split into service/timer files, then:
sudo systemctl daemon-reload
sudo systemctl enable --now tuishou-auto-update.timer
```

A worked example (split files):

```ini
# /etc/systemd/system/tuishou-auto-update.service
[Unit]
Description=tuishou scheduled auto-update (one cycle, opt-in)
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
ExecStart=/usr/local/bin/tuishou auto-update run --once
EnvironmentFile=-/etc/tuishou/auto-update.env
```

```ini
# /etc/systemd/system/tuishou-auto-update.timer
[Unit]
Description=Run tuishou auto-update on a schedule

[Timer]
OnUnitActiveSec=10min
RandomizedDelaySec=1min
Persistent=true

[Install]
WantedBy=timers.target
```

## Status

```sh
tuishou auto-update status --json
```

Reports `enabled`, `last_check`, `interval`, `policy`, `controller_policy`, hold state (`active`, `reason`, `expires_at`), and per-component `last_result`, `reason`, `installed_version`, `last_version`, `next_eligible_version`. The JSON shape is intended for automation/Ansible.

## Safety summary

- Opt-in only; off by default.
- Unknown config keys are errors.
- Interval below 5 minutes is rejected.
- Attestation is mandatory in auto mode; `--allow-unattested` is refused.
- Releases are fetched only from GitHub / GitHub release asset hosts over HTTPS; redirects off those hosts are refused.
- Download size caps and safe archive extraction are the same as manual installs.
- Only the final installed binary `--version` smoke check executes code from a release.
- All state is kept under one directory with mode `0700`.
