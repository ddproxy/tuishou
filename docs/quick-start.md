# Quick start

`tuishou` installs, updates and rolls Tuishou components from GitHub releases.

## 1. Install tuishou

Download the `tuishou` release archive for your target, verify its SHA-256 checksum, and install the binary on your `PATH`.

## 2. Install a component

```sh
tuishou install lu
```

`tuishou` resolves the latest release, downloads the archive and checksum, verifies integrity, verifies build attestation when `gh` is available, extracts the expected binaries, and installs them atomically.

## 3. Check status

```sh
tuishou status
```

## 4. Update

```sh
tuishou update --check
tuishou update
```
