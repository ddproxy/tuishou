# Node update procedure

`tuishou` runs on every node. Nothing pushes updates to nodes and there is no
remote roll command: each node pulls, verifies, drains, installs, restarts,
checks health and rolls back on its own. `tuishou update` and the opt-in
`tuishou auto-update run` are the only update paths.

## What one cycle does

For the Lu agent and Reiki, a component update follows this order:

1. **Drain.** `lu drain <node>` marks the node draining. `tuishou` then polls
   `lu nodes --json` until the node reports `draining: true` with no active job,
   subject to `[auto_update].drain_timeout` (default `5m`). A timeout aborts
   before any binary is replaced.
2. **Install.** The release archive is verified in process (SHA-256 plus the
   GitHub attestation) and the new binary is installed atomically; the previous
   version is kept as a backup.
3. **Restart.** The relevant systemd unit is restarted:
   `lu-agent.service` for the Lu agent, the configured Reiki unit for Reiki.
   Reiki is always applied **before** the Lu agent when both are due, even if
   `[auto_update].components` lists them the other way around.
4. **Health check.** `tuishou` polls `lu nodes --json` for the exact new version
   and `lu status --json` for controller health, subject to
   `[auto_update].health_timeout` (default `5m`). Reiki additionally runs
   `reiki check --config <path>` before and after its restart.
5. **Rollback on failure.** A failed install, check, restart or health check
   restores the previous binary, restarts the unit, verifies the old version is
   healthy, and undrains the node. The bad version is marked so later cycles
   skip it. If health cannot be restored, the node stays drained so no new
   work is accepted.
6. **Undrain.** On success, `lu undrain <node>` returns the node to service. A
   failed undrain is reported as a failure rather than hidden.

Every `lu` and `systemctl` child runs with an argument vector and a hard
deadline (`[auto_update].command_timeout`, default `30s`); a hung child is
killed and reaped.

## Run it by hand on a node

Install `tuishou` on the node, place a trusted `tuishou.toml` next to the
invocation (or pass `--config`), then run:

```sh
tuishou update --check          # optional: show what would be updated
tuishou update                  # update all installed components
tuishou update reiki            # or one component at a time
tuishou status --json           # inspect the result and state
```

`--prefix` selects an install prefix; its `.tuishou.json` state lives under the
prefix. A manual `update` honours the `[update]` settings below. A failing
health check exits non-zero and writes a receipt under the state directory;
use `tuishou rollback <component>` if you need to restore a version yourself.

## Local controls

Both `update` and `auto-update run` read `[update]`:

```toml
[update]
# Wait a uniformly random time in 0..=start_delay_max before starting.
# Default "0"; use "10m" on fleets so nodes do not all act at once.
start_delay_max = "10m"

# Skip a cycle when this many nodes are already draining or offline.
# Unset by default, which disables the check.
max_unavailable = 1
```

`max_unavailable` asks the controller for `lu nodes --json` before the cycle
does any work. Nodes reported as draining or offline count against the limit.
When the number already meets the limit, the node reports
`skipped (fleet limit)` for each component and tries again on the next cycle.
If the controller state cannot be read, the cycle fails closed and is skipped
too. With `max_unavailable` unset, the controller is not consulted.

## Scheduling auto-update

Auto-update is opt-in and **not recommended for production**: it installs new
releases without human review. To enable it on a single node, set
`[auto_update] enabled = true` in `tuishou.toml`, then inspect the timer unit
it would use:

```sh
tuishou auto-update install-timer --interval 10m
```

The command prints the systemd service and timer; it never installs them.
Schedule them with your configuration management if you accept unattended
updates. See [auto-update.md](auto-update.md) for the full configuration.

## Updating many nodes

There is no central host that drives the update. Run `tuishou update` on each
node (or schedule `auto-update` there) and use two controls to keep the fleet
available:

- `start_delay_max` adds a random delay at the start of every cycle, so
  concurrent schedules do not turn into a thundering herd. A value of `10m` is
  recommended for fleets; the default `0` starts immediately.
- `max_unavailable` reads the controller's node list and skips the cycle when
  too many nodes are already draining or offline. Skipped nodes retry on their
  next cycle. Combine it with a short scheduler interval so retries happen
  promptly. Failure to read the controller state also skips the cycle.

An example Ansible task installs `tuishou` on every node and runs the update
locally, with no SSH orchestration from tuishou itself:

```yaml
- name: Update the Tuishou stack on every node
  become: true
  block:
    - name: Install tuishou
      ansible.builtin.get_url:
        url: "https://github.com/ddproxy/tuishou/releases/download/v0.2.1/tuishou-v0.2.1-{{ tuishou_target }}.tar.gz"
        dest: "/tmp/tuishou-v0.2.1-{{ tuishou_target }}.tar.gz"
        mode: "0644"

    - name: Unpack tuishou
      ansible.builtin.unarchive:
        src: "/tmp/tuishou-v0.2.1-{{ tuishou_target }}.tar.gz"
        dest: "/usr/local/bin"
        remote_src: true
        include: ["tuishou"]
        mode: "0755"

    - name: Deploy node config
      ansible.builtin.copy:
        src: "tuishou.toml"
        dest: "/etc/tuishou.toml"
        owner: root
        group: root
        mode: "0644"

    - name: Run the local update
      ansible.builtin.command:
        argv:
          - "/usr/local/bin/tuishou"
          - "--config"
          - "/etc/tuishou.toml"
          - "update"
      changed_when: false
```

Set `tuishou_target` per host (for example
`x86_64-unknown-linux-musl`) and keep the config owned by root or the running
user; group- or world-writable configs are refused.
