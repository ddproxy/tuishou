# Scheduled auto-update (opt-in)

> **Not recommended for production.** Auto-update installs new releases without human review. It exists for fleets that update very often and accept that risk. It is off by default and every safeguard below is designed to make accidental enablement difficult.

## What it does

`tuishou auto-update` runs one update cycle at a time. The scheduler is external: a systemd timer or cron invokes `tuishou auto-update run --once`. There is no long-running daemon.

For each configured component the cycle:

1. Takes an exclusive OS advisory lock on `<state dir>/lock` (kernel-released on exit); a second concurrent run exits quietly. The same lock guards hold and state-changing commands.
2. Lists GitHub releases with conditional requests (`ETag`) and honours rate limits / `Retry-After`. `GITHUB_TOKEN`/`GH_TOKEN` is attached only to GitHub API requests, never to asset hosts, and is stripped on cross-origin redirects.
3. Selects the newest eligible release by full version precedence (including pre-release identifiers), then applies policy, channel, `min_release_age`, `max_version`, bad-version marks and hold state. A release without a usable `published_at` is skipped when `min_release_age` is configured.
4. Applies the same verified install path as manual updates: checksum, GitHub attestation, safe extraction, atomic replace, `.prev`, and optional `--version` smoke check.
5. Writes single-line journal events and a state file for `tuishou auto-update status`.

## Enabling (opt-in only)

Auto-update never runs unless the config explicitly enables it:

```toml
[auto_update]
enabled = true                 # default false; required
prefix = "/opt/tuishou"        # optional; every auto-update command uses it
interval = "10m"               # 5m..15m recommended, below 5m rejected
components = ["lu", "ting"]
policy = "patch"               # patch | minor | any
min_release_age = "10m"        # release must be this old before install
channel = "release"            # release | prerelease
require_attestation = true     # cannot be false in auto mode
attestation_signer = "ddproxy/tuishou-lu/.github/workflows/release.yml"
controller_policy = "when_idle" # manual | when_idle | window
controller_unit = "lu.service"  # systemd unit restarted after a controller update
agent_unit = "lu-agent.service" # systemd unit restarted after an agent update
command_timeout = "30s"         # hard deadline for each lu/systemctl child process
backup_dir = "/var/backups/tuishou"
backup_keep = 5
# node_name = "node-1"           # otherwise TUISHOU_NODE_NAME / HOSTNAME
# max_version = "v0.5.0"
# notify_command = ["/usr/local/bin/notify", "tuishou"]
# notify_after_failures = 3
```

Unknown keys in `tuishou.toml` are errors. `--allow-unattested` is refused in auto mode and `gh` must be on `PATH`. Use the global `--config <path>` flag or `TUISHOU_CONFIG` to select a different config file; generated systemd units always pass an absolute path.

`--prefix` is available on every `auto-update` action and on `adopt`. The flag overrides `auto_update.prefix`; without either, state uses the XDG state directory. The resolved prefix scopes installed state, HOLD, locks and results, and the generated service passes it to `auto-update run` as an absolute path.

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

For `ting` or a component that owns the agent binaries, auto-update first lists the checksum-verified archive contents. If the release contains none of the component's expected binaries, the cycle records `skipped` with `not in this release` and does not drain, install or restart anything. Otherwise it uses the real Lu drain path:

1. `lu drain <name>`
2. poll `lu nodes --json` until the named node explicitly reports `draining: true` and no `current_job`, up to `drain_timeout`
3. verified install of the agent binaries (`ting`, `lu-agent`); the controller binary `lu` is never replaced by this path
4. `systemctl restart <agent_unit>`
5. poll `systemctl is-active --quiet <agent_unit>` and `lu nodes --json` until the named node reports the exact new `version` (release tags are normalised; `0.4.10` does not match `0.4.1`)
6. `lu undrain <name>`

`lu node <name> --json` is host statistics only and is never used for version or drain state. If the node does not become healthy, tuishou records the version as bad, restores `.prev`, restarts the unit, verifies that the old version is live again, then undrains. If any recovery step fails, the node stays drained and the cycle exits non-zero; an undrain failure is always propagated.

## Controller updates

Controller updates are conservative:

- `controller_policy = "manual"` (default): never auto-update the controller.
- `when_idle`: update only when `lu status --json` reports `jobs.leased`, `jobs.running`, `jobs.recovering` and `jobs.needs_input` all present as non-negative integers and all zero. Any missing, malformed or failed status skips the update.
- `window`: like `when_idle`, but only inside the configured UTC window.

Before installing, tuishou creates a `0700` `backup_dir` (if needed) and runs `lu backup --config <absolute config path> --out <unique file>` against the same configuration the CLI loaded. The output file is created with mode `0600` and a name like `lu-backup-20261122T101500Z-<random>.sql`. A failed command or an empty output file refuses the update; old backups are then pruned until `backup_dir` holds at most `backup_keep` files. After install it restarts `controller_unit` and waits for `lu status --json` to report the exact new version before recording state. A health failure records the bad version, restores `.prev`, restarts and verifies restored health; recovery failures are reported with the real command error text.

Schema changes are never crossed automatically. Every release between the installed version and the candidate must have release notes; missing notes fail closed. If any of those release notes contain the line `Requires: new controller database`, or the change is a 0.y minor under `patch`, auto-update skips with a clear logged reason.

## systemd timer

`tuishou auto-update install-timer` prints a service and timer unit pair; it does not install them. The timer uses both `OnBootSec` and `OnActiveSec`, defaulting to the validated `auto_update.interval` (10 minutes unless configured); `--interval` overrides it, and both are rejected below 5 minutes. It also emits `RandomizedDelaySec` jitter and `Persistent=true`. The service runs `tuishou --config <absolute path> auto-update run --once` so it does not depend on the caller's working directory, and appends `--prefix <absolute path>` when a prefix is configured or passed on the command line.

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
ExecStart=/usr/local/bin/tuishou --config /etc/tuishou/tuishou.toml auto-update run --once --prefix /opt/tuishou
EnvironmentFile=-/etc/tuishou/auto-update.env
```

```ini
# /etc/systemd/system/tuishou-auto-update.timer
[Unit]
Description=Run tuishou auto-update on a schedule

[Timer]
OnBootSec=10min
OnActiveSec=10min
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
- Releases are fetched only from GitHub / GitHub release asset hosts over HTTPS (plus explicitly configured allowed hosts); credentials are stripped on origin changes and redacted from logs, notifications and errors.
- Download size caps and safe archive extraction are the same as manual installs.
- Only the staged binary `--version` smoke check executes code from a release, before any installed binary or `.prev` file is changed.
- All state is kept under one directory with mode `0700`.
