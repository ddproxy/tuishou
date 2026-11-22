# Dogfooding tuishou

This guide covers adopting tuishou on nodes that already have `lu`/`ting` installed by hand, and installing tuishou on a fresh node.

## Existing node (adopt, no replacement)

If a node already has working binaries installed manually, record them in tuishou state without replacing files:

```sh
tuishou adopt lu --version v0.4.0 --prefix /usr/local/bin
tuishou adopt ting --version v0.4.0 --prefix /usr/local/bin
tuishou status --json --prefix /usr/local/bin
```

`adopt` checks that at least one expected component binary exists in the install directory, records the version/target/checksum in state, and does not download or replace binaries. It is safe to run before the first managed update.

## Fresh node

1. Install `tuishou` itself from a verified release (see [Verify a tuishou release](verify-release.md)).
2. Install components:

```sh
tuishou install lu --version v0.4.0
tuishou install ting --version v0.4.0
```

3. Confirm state:

```sh
tuishou status --json
```

## Component naming

The default component table points at `ddproxy/tuishou-lu`. Release assets use the real naming scheme:

- `lu-<tag>-<target>.tar.gz`
- `lu-<tag>-<target>.tar.gz.sha256`

The `lu` component installs `lu`, `ting`, and accepts `lu-agent` while the rename lands. The `ting` component installs the node agent binaries from the same archive.

## Ansible snippet

`tuishou status --json` is stable and machine-readable. Example Ansible task that adopts an existing binary only when it is missing from state:

```yaml
- name: Read tuishou status
  command: tuishou status --json --prefix /usr/local/bin
  register: tuishou_status
  changed_when: false
  failed_when: false

- name: Adopt existing lu binary
  command: tuishou adopt lu --version {{ lu_version }} --prefix /usr/local/bin
  when: >-
    (tuishou_status.stdout | default('[]') | from_json)
    | selectattr('component', 'equalto', 'lu')
    | selectattr('installed_version')
    | list
    | length == 0
```

For scheduled updates, keep auto-update off unless the fleet explicitly opts in and the risks in [auto-update.md](auto-update.md) are acceptable.
