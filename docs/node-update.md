# Node update procedure

`tuishou` runs on every node. Nothing pushes updates to nodes and there is no
remote roll command: each node pulls, verifies, drains, installs, restarts,
checks health and rolls back on its own. `tuishou update` and the opt-in
`tuishou auto-update run` are the only update paths.

A Lu release contains both `lu` (the controller) and `ting` (the node agent).
They are installed, backed up as `.prev`, and rolled back as a pair. The role is
never guessed from archive contents: `tuishou` restarts the unit that is both
installed (`systemctl cat`) and enabled (`systemctl is-enabled --quiet`). The
defaults are `ting.service` for the agent and `lu-controller.service` for the
controller; both or neither qualifying fails closed.

## What one cycle does

For the agent and Reiki on a node running Lu:

1. **Drain.** `lu drain <node>` is issued only when the controller is reachable.
   `tuishou` then polls `lu nodes --json` until the node reports `draining: true`
   with no active job, subject to `[auto_update].drain_timeout` (default `30m`).
   A timeout aborts before any binary is replaced and undrains the node.
2. **Stage and install.** The release archive is verified in process (SHA-256
   plus the GitHub attestation). Reiki's new binary is checked with
   `reiki check --config <path>` **before** any swap; a failure keeps the old
   binary and marks the candidate bad. The new pair is installed atomically and
   the previous pair is kept as `.prev`.
3. **Restart.** The detected systemd unit is restarted: `ting.service` for the
   agent, `lu-controller.service` for the controller, the configured Reiki unit
   for Reiki. Reiki is always applied **before** the Lu agent when both are due,
   even if `[auto_update].components` lists them the other way around.
4. **Health check.** The agent must appear in `lu nodes --json` at the exact new
   version with a heartbeat no older than `heartbeat_max_age` (default `5m`).
   The controller must report the new version in `lu status --json`. Reiki
   additionally requires an active unit, `reiki check` after restart, and a
   healthy `reiki_health_url` admin endpoint (default
   `http://127.0.0.1:8080/health`). Polls are bounded by `health_timeout`
   (default `5m`).
5. **Rollback on failure.** A failed check, restart or health check restores
   the previous pair, restarts the unit, verifies the old version is healthy,
   marks the new version bad and records the result in status. Undrain is
   attempted on every failure path, including a failed recovery.
6. **Undrain.** On success, `lu undrain <node>` returns the node to service. A
   failed undrain is reported as a failure rather than hidden.

For the controller, `[auto_update].controller_policy` defaults to `manual`, so
controller updates are skipped until set to `when_idle` or `window`. Before any
swap, `lu backup --out <file>` must produce a non-empty backup under
`backup_dir`; a failed backup refuses the update. `lu status --json` is then
polled until no jobs are leased, running, recovering or needing input, up to
`controller_wait_timeout` (default `30m`). A release whose notes signal a new
database schema also skips the controller for manual migration.

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
tuishou status --json           # installed_version/latest_version/last_check/last_result/bad_versions
```

`--prefix` selects an install prefix; its `.tuishou.json` state lives under the
prefix. A manual `update` honours the `[update]` settings below. A failing health
check exits non-zero and writes a receipt under the state directory; use
`tuishou rollback <component>` if you need to restore a version yourself.
Manual `--allow-unattested` is available for direct installs but is never used
by auto-update, which always verifies attestations.

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
releases without human review. Two switches are required: `[auto_update]
enabled = true` and `auto_update = true` in each `[[component]]` block you want
scheduled. `[auto_update].components` does not opt components in. Only the
`stable` channel is accepted; prereleases are refused.

The shipped `contrib/tuishou-update.service` and `contrib/tuishou-update.timer`
run one cycle with a randomized delay and `Persistent=true`. Set the config path
in `ExecStart` and the interval in the timer, install both units, then enable
the timer:

```sh
systemctl enable --now tuishou-update.timer
```

`tuishou auto-update install-timer --interval 10m` prints an equivalent unit
pair (it never installs them). To enable only the agent on a node, set
`auto_update = true` on `[[component]] name = "lu"` and leave the other
components off; the controller stays manual until `controller_policy` is
relaxed and a backup directory is configured. See [auto-update.md](auto-update.md)
for the full configuration.

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

The timer's `RandomizedDelaySec` provides a second layer of fleet jitter. An
example Ansible task installs `tuishou` on every node and runs the update
locally, with no SSH orchestration from tuishou itself. It pins the bootstrap
archive digest from an independently controlled record before the installer is
allowed to extract anything. A `.sha256` asset from the same release, or an
in-process attestation run by the binary being installed, is not a trust
bootstrap:

```yaml
- name: Update the Tuishou stack on every node
  become: true
  block:
    - name: Read the pinned archive digest from trusted records
      ansible.builtin.uri:
        url: "{{ tuishou_bootstrap_digest_url }}"
        return_content: true
      register: tuishou_bootstrap
      delegate_to: localhost
      become: false

    - name: Set the pinned digest
      ansible.builtin.set_fact:
        tuishou_expected_sha256: "{{ tuishou_bootstrap.content | trim | split | first | lower }}"

    - name: Refuse a malformed pinned digest
      ansible.builtin.assert:
        that:
          - tuishou_expected_sha256 is match('^[0-9a-f]{64}$')

    - name: Fetch the installer
      ansible.builtin.get_url:
        url: "https://raw.githubusercontent.com/ddproxy/tuishou/main/install.sh"
        dest: "/tmp/tuishou-install.sh"
        mode: "0600"

    - name: Install tuishou with the pinned digest
      ansible.builtin.command:
        argv:
          - "bash"
          - "/tmp/tuishou-install.sh"
          - "--version"
          - "v0.3.1"
          - "--no-components"
          - "--prefix"
          - "/usr/local"
          - "--yes"
          - "--expect-sha256"
          - "{{ tuishou_expected_sha256 }}"
      changed_when: false

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

Set `tuishou_bootstrap_digest_url` to a record you control independently of the
GitHub release, and keep the config owned by root or the running user;
group- or world-writable configs are refused. Ansible's `become` supplies
privilege for the task; the installer itself never invokes `sudo`. Mandatory
pinned digests and signed packages remain a residual design decision.
