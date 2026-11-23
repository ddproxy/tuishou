# Hosted TUIs (`tuishou top`)

`tuishou top` runs `reiki top` and `lu top` in one terminal. Both children start
at launch, each in its own pseudo-terminal with its own vt100 screen. Only the
active screen is painted; the other process keeps running, so its selection,
scroll position and filters survive a swap without a reload. Resize events are
forwarded to both pseudo-terminals.

## Keys

| Key | Action |
| --- | --- |
| `Alt+1` / `F1` | select Reiki |
| `Alt+2` / `F2` | select Lu |
| `Ctrl+Tab` | cycle between the two tabs |
| everything else | sent to the active child |
| `Ctrl+b` then `r` | restart the active child |
| `Ctrl+b` then `q` | quit the host and both children |
| `Ctrl+b` then `?` | show help; any key closes it |
| `Ctrl+b` then `Esc` | cancel the prefix |

The tab bar is one line: the active tab is highlighted and an exited child gets
an `!`. Mouse events are forwarded only while the active child asks for mouse
tracking.

## Commands and configuration

By default the host resolves each child binary from the component registry
(the configured install prefix plus `reiki` or `lu`), appends `top`, and adds
the component's configured `--config` path where one exists. Override the whole
argv with flags or config:

```sh
tuishou top \
  --reiki-cmd 'reiki top --config /etc/reiki/reiki.toml' \
  --lu-cmd 'lu top --url http://127.0.0.1:8080'
```

```toml
[top]
reiki_cmd = ["reiki", "top", "--config", "/etc/reiki/reiki.toml"]
lu_cmd = ["lu", "top", "--url", "http://127.0.0.1:8080"]
```

A CLI flag wins over config, and config wins over the last argv saved in state.
Override strings are argument vectors, not shell commands: quotes group
arguments, backslash escapes the next character, and nothing is expanded.
Credentials belong in the child's own environment variables (for example
`REIKI_TOKEN` or `LU_TOKEN`), not in argv. The host does not log argv or
environment values; known token values are redacted from its diagnostics.

If a child binary is missing, cannot start, or exits because it lacks the
`tui` feature, the host prints the exit status and how to install a suitable
build (`tuishou install <component>`, or a source build with the component's
`tui` feature), plus `Ctrl+b r` to retry.

## State and shutdown

The host records the active tab, per-child restart counts and last argv in
`$XDG_STATE_HOME/tuishou/top.json` (mode `0600`), or
`<prefix>/.tuishou-top.json` when a prefix is configured. The active tab is
restored on the next launch. Child-internal state beyond the live processes is
not persisted by the host.

On every exit path — `Ctrl+b q`, panic, `SIGTERM`, `SIGHUP` or `SIGINT` — the
host restores raw mode and the alternate screen, sends `SIGHUP` to both child
process groups and follows with `SIGKILL` after a grace period if they are
still alive. No child is left behind.

## Build options

The `top` feature pulls in `ratatui`, `crossterm`, `portable-pty`, `tui-term`,
`vt100` and (on Unix) signal helpers. It is enabled by default, including
release builds. Minimal builds can exclude it:

```sh
cargo build --no-default-features --locked
```

With the feature disabled, `tuishou top` exits with a usage error explaining
how to rebuild; all other commands are unchanged.
