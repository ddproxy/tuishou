# Fleet roll

`tuishou roll` performs a controlled fleet roll over SSH.

```sh
tuishou roll --nodes <file|list> [--component ting] [--version vX.Y.Z] [--batch N] [--dry-run] [--force-after SECONDS]
```

## Inventory format

Inventory files are TOML. Each node has a name, SSH target, optional port, and optional unit name.

```toml
[[node]]
name = "node-1"
ssh = "deploy@10.0.0.11"
port = 22
unit = "lu-agent.service"

[[node]]
name = "node-2"
ssh = "deploy@10.0.0.12"
unit = "lu-agent.service"
```

## Roll sequence

For each node, in order:

1. Drain the node with `lu drain <name>`.
2. Poll `lu nodes --json` until the named node explicitly reports `draining: true` and no `current_job`.
3. Run `tuishou install` on the node over SSH.
4. Restart the service unit.
5. Poll `lu nodes --json` until the named node reports the exact new `version`.
6. Undrain the node with `lu undrain <name>`.

`lu node <name> --json` is host statistics only; it is not used for drain or version checks.

A failure stops the roll and leaves later nodes untouched. Use `--dry-run` to print the plan.
