# Auto-update

`tuishou` can check GitHub releases, verify the matching artifacts in process, download
them, and apply them without a system service. It is off by default.

## Enable it

```toml
[auto_update]
enabled = true
interval = "10m"
components = ["reiki", "lu", "tuishou"]
policy = "patch"
min_release_age = "10m"
channel = "release"
require_attestation = true
node_name = "node-1"
reiki_config_path = "/var/lib/reiki/reiki.toml"

[verify]
signer_workflow = ".github/workflows/release.yml"
cache_ttl = "24h"
```

`interval` and the other durations use Go-style values such as `10m` or `1h`.
`components` defaults to the three built-ins in table order; cycles still apply Reiki
before Lu even if the config lists them the other way around. See
[components.md](components.md).

## What happens

1. GitHub's release API is checked for each configured component. Release-channel
   rules, version policy, minimum age, held versions and bad-version marks are shared
   with `status` and `update --check`.
2. Each candidate asset is verified against its GitHub attestation in process:
   artifact digest, Sigstore/Fulcio chain, Rekor SET and inclusion proof, signing-time
   validity, and the GitHub OIDC workflow identity and SLSA provenance. A TUF refresh
   may contact `tuf-repo-cdn.sigstore.dev`.
3. A component whose required binaries are missing from the release is reported as
   `not in this release`; it is never counted as an update.
4. Reiki is drained through `lu drain <node>` on nodes running Lu, checked with
   `reiki check --config <path>`, restarted, health-checked and undrained. A failure
   restores the previous binary, restarts, confirms health, undrains, and marks the
   version bad.
5. Every component gets exactly one status line from the fixed vocabulary in
   [components.md](components.md). `--json` reports the same data.

Verification failures never hide other components: a cycle reports all configured
components and exits 1 when any of them failed.

`--allow-unattested` is rejected by auto-update: it never downgrades to unverified
installs.

## State and receipts

By default, state is under `~/.local/state/tuishou` (or `$XDG_STATE_HOME/tuishou`).
Use `tuishou auto-update status` to inspect the last check, holds, bad versions and
per-component results. With `--prefix`, state lives under `<prefix>/.tuishou`.

The verifier cache defaults to a `0700` directory under the state directory unless
`[verify].cache_dir` is set.

## Scope

Auto-update covers only the built-in components. It does not support third-party
components, and it runs locally on the node where it is scheduled. See
[node-update.md](node-update.md) for the single-node procedure, fleet settings and
scheduling guidance.
