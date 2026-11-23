# Operations

For operators running tuishou after installation.

## Update

`tuishou update --check` prints one line per component and exits `10` when an update exists, `1` on a failed check, `2` on usage error and `0` when everything is current. `tuishou update` updates installed components; `tuishou update reiki` can install a missing component explicitly.

Reiki and Lu installs use the local apply paths: unit detection, config check, drain and health checks, then state commit. A failed health check restores `.prev` and marks the version bad for auto-update. Manual `tuishou install` never uses `--allow-unattested`; pass it explicitly only when you accept the downgrade.

## Rollback and backups

`tuishou rollback <component>` restores `<name>.prev` in the install bin directory. The installer keeps `tuishou.prev` for the bootstrap binary. A missing `.prev` is an error, not a silent success.

Controller auto-update with `controller_policy` other than `manual` requires `backup_dir` and runs `lu backup --out <FILE>` before replacing the controller. Tuishou does not provide a database restore command; use the Lu tooling for restore. See [node-update](node-update.md).

## Logs, health and state

Structured lines are written to stderr as `tuishou: event=... key=value` and captured by systemd. Useful locations:

| Item | Location |
| --- | --- |
| Install state | `<prefix>/.tuishou.json`, else `$XDG_STATE_HOME/tuishou/installed.json` |
| Auto-update state | `<prefix>/.tuishou/` (`auto.json`, `HOLD`, `lock`) |
| Verify cache | `<prefix>/verify-cache`, else state parent plus `verify-cache` |
| Health endpoint | `auto_update.reiki_health_url`, default `http://127.0.0.1:8080/health` |

Health checks use https or loopback http only. `tuishou status --json` gives machine-readable versions and statuses.

## Auto-update

Auto-update is off by default and not recommended for production.

1. Set `[auto_update] enabled = true` in `tuishou.toml`.
2. Run `tuishou auto-update install-timer --interval 10m`; the command prints a service and timer unit pair, it does not install them.
3. Install those units on the host and enable the timer.
4. Use `tuishou auto-update status --json`, `hold --reason`, `release` and `clear-bad` to operate it.

Auto-update refuses `require_attestation = false` and the `--allow-unattested` flag. It accepts stable releases only. See [auto-update](auto-update.md).

## Command not present

`tuishou top` is not implemented in v0.3.0. The installer lists it as planned in its next-steps block. Use `tuishou status` for current state.
