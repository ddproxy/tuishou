# FAQ

## Where is install state stored?

By default: `$XDG_STATE_HOME/tuishou/installed.json`, or `~/.local/state/tuishou/installed.json` when `XDG_STATE_HOME` is unset. With `--prefix DIR`, state is stored at `<DIR>/.tuishou.json`.

## What exit codes does tuishou use?

- `0`: success.
- `1`: error.
- `10`: `tuishou update --check` found updates.

## Does update cross major versions?

No. Pass `--allow-major` to allow a major version change.

## What happens if gh is not installed?

`tuishou` reports that attestation verification requires `gh`. Pass `--allow-unattested` to continue with checksum verification only.

## How do I add a component?

Add a `[[component]]` entry to `tuishou.toml` in the working directory. Unknown config keys are errors.

```toml
[[component]]
name = "example"
repo = "owner/example"
asset_pattern = "example-{tag}-{target}.tar.gz"
binaries = ["example"]
service_unit = "example.service"
```
