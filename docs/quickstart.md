# Quickstart

For new operators who want one checkable result in ten minutes.

Assumptions: Linux x86_64, `bash`, `curl`, `tar` and `sha256sum` available. A normal user install goes to `~/.local`; root installs to `/usr/local`.

1. Install and verify the stack.

   ```sh
   curl -fsSL https://raw.githubusercontent.com/ddproxy/tuishou/main/install.sh | bash -s -- --components reiki,lu
   # expect: PASS: attestation: PASS (tuishou verify
   ```

   The installer checks the archive checksum, installs `tuishou`, verifies the archive with `tuishou verify`, then installs `reiki` and `lu`. A failed attestation removes the binary and exits 4. This one-line form is for evaluation only: it cannot verify the verifier before running it. Production installs must obtain the bootstrap digest from an independent trusted record and pass `--expect-sha256 <digest>` to the installer before any extraction, as shown in `docs/install.md`.

2. Put the user install on `PATH` (skip for root).

   ```sh
   export PATH="$HOME/.local/bin:$PATH"
   ```

3. Prove the install.

   ```sh
   tuishou status
   # expect: reiki: up to date (
   ```

   One line per component uses the fixed vocabulary: `up to date (...)`, `update available ...`, `not installed (...)` or `failed (...)`.

4. Next commands, from the installer summary:

   ```text
   tuishou status
   tuishou top
   tuishou auto-update install-timer --interval 10m
   ```

   `tuishou top` is reserved and not implemented in v0.3.2. Auto-update needs `[auto_update] enabled = true` and systemd units installed by the operator; `install-timer` only prints them.

Full command reference: [cli.md](cli.md). Install options: [install.md](install.md).
