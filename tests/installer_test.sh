#!/usr/bin/env bash
# Installer tests against a local fake GitHub release server.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
installer="$root/install.sh"
work=$(mktemp -d)

# shellcheck source-path=SCRIPTDIR source=fixtures/installer/fake_release_server.sh
source "$root/tests/fixtures/installer/fake_release_server.sh"
trap 'fixture_stop_server; rm -rf "$work"' EXIT
fixture_init "$work"

case_no=0
section() {
  case_no=$((case_no + 1))
  printf '\n[%d] %s\n' "$case_no" "$1"
}

expect_exit() {
  local expected="$1" got=0
  shift
  set +e
  "$@" > "$work/case.out" 2>&1
  got=$?
  set -e
  cat "$work/case.out"
  if [[ $got -ne $expected ]]; then
    printf 'expected exit %d, got %d\n' "$expected" "$got" >&2
    return 1
  fi
}

assert_contains() {
  local text="$1" needle="$2"
  if [[ $text != *"$needle"* ]]; then
    printf 'expected output to contain: %s\n' "$needle" >&2
    printf '%s\n' "$text" >&2
    return 1
  fi
}

assert_absent() {
  if [[ -e $1 ]]; then
    printf 'expected no file at %s\n' "$1" >&2
    return 1
  fi
}

make_malicious_release() {
  local tag="$1" archive="$2" checksum="$3"
  fixture_write_release "$FIX_SERVER_ROOT/repos/$FIX_REPO/releases/tags/$tag" \
    "$tag" "$archive" "$checksum"
}

section "usage error"
out=$(expect_exit 2 fixture_installer_env bash "$installer" --components bogus --prefix "$work/usage")
assert_contains "$out" "unknown component 'bogus'"
assert_contains "$out" "known components: reiki, lu"

section "unsupported platform"
out=$(export TUISHOU_FAKE_UNAME=aarch64; expect_exit 3 fixture_installer_env bash "$installer" --prefix "$work/unsupported")
assert_contains "$out" "unsupported platform: linux/aarch64"
assert_contains "$out" "x86_64-unknown-linux-musl"
assert_absent "$work/unsupported"

section "dry run"
out=$(fixture_installer_env bash "$installer" --prefix "$work/dry" --yes --dry-run --components reiki,lu)
assert_contains "$out" "dry run: no changes will be made"
assert_contains "$out" "tuishou: $FIX_GOOD_TAG"
assert_contains "$out" "tuishou status"
assert_absent "$work/dry"

section "checksum mismatch"
out=$(expect_exit 4 fixture_installer_env bash "$installer" --prefix "$work/bad-checksum" --yes --version "$FIX_BAD_TAG")
assert_contains "$out" "checksum mismatch"
assert_absent "$work/bad-checksum/bin/tuishou"

section "pinned digest format"
out=$(expect_exit 4 fixture_installer_env bash "$installer" --prefix "$work/pinned-format" --yes --expect-sha256 "not-a-digest")
assert_contains "$out" "--expect-sha256 must be 64 hexadecimal characters"
assert_absent "$work/pinned-format"

section "pinned digest mismatch"
good_digest=$(sha256sum "$FIX_SERVER_ROOT/assets/$FIX_ARCHIVE_GOOD" | awk '{ print $1 }')
wrong_digest=$(printf '%s' "${good_digest}-mismatch" | sha256sum | awk '{ print $1 }')
out=$(expect_exit 4 fixture_installer_env bash "$installer" --prefix "$work/pinned-bad" --yes --expect-sha256 "$wrong_digest")
assert_contains "$out" "pinned SHA-256 mismatch"
assert_absent "$work/pinned-bad/bin/tuishou"

section "pinned digest succeeds and is case-insensitive"
out=$(fixture_installer_env bash "$installer" --prefix "$work/pinned-good" --yes --no-components --expect-sha256 "${good_digest^^}")
assert_contains "$out" "pinned SHA-256: PASS"
assert_contains "$out" "attestation: PASS (tuishou verify"
assert_contains "$out" "checksum: PASS"
[[ -x $work/pinned-good/bin/tuishou ]]

section "pinned digest keeps the release checksum as a consistency check"
bad_digest=$(sha256sum "$FIX_SERVER_ROOT/assets/$FIX_ARCHIVE_BAD" | awk '{ print $1 }')
out=$(expect_exit 4 fixture_installer_env bash "$installer" --prefix "$work/pinned-checksum" --yes --version "$FIX_BAD_TAG" --expect-sha256 "$bad_digest")
assert_contains "$out" "checksum mismatch"
assert_absent "$work/pinned-checksum/bin/tuishou"

section "archive with symlink entry is rejected"
mal_tag="v9.9.7"
mal_archive="tuishou-$mal_tag-x86_64-unknown-linux-musl.tar.gz"
mal_checksum="$mal_archive.sha256"
mal_root="$work/mal-symlink/root"
mkdir -p "$mal_root"
ln -s /bin/sh "$mal_root/tuishou"
tar -czf "$FIX_SERVER_ROOT/assets/$mal_archive" -C "$mal_root" tuishou
sha256sum "$FIX_SERVER_ROOT/assets/$mal_archive" |
  awk -v name="$mal_checksum" '{ print $1 "  " name }' > "$FIX_SERVER_ROOT/assets/$mal_checksum"
make_malicious_release "$mal_tag" "$mal_archive" "$mal_checksum"
out=$(expect_exit 5 fixture_installer_env bash "$installer" --prefix "$work/mal-symlink" --yes --version "$mal_tag")
assert_contains "$out" "must be a regular file"
assert_absent "$work/mal-symlink/bin/tuishou"

section "archive with absolute entry name is rejected"
mal_tag="v9.9.6"
mal_archive="tuishou-$mal_tag-x86_64-unknown-linux-musl.tar.gz"
mal_checksum="$mal_archive.sha256"
mal_root="$work/mal-absolute/root"
mkdir -p "$mal_root"
printf 'untrusted\n' > "$mal_root/evil"
tar -czf "$FIX_SERVER_ROOT/assets/$mal_archive" --absolute-names "$mal_root/evil"
sha256sum "$FIX_SERVER_ROOT/assets/$mal_archive" |
  awk -v name="$mal_checksum" '{ print $1 "  " name }' > "$FIX_SERVER_ROOT/assets/$mal_checksum"
make_malicious_release "$mal_tag" "$mal_archive" "$mal_checksum"
out=$(expect_exit 5 fixture_installer_env bash "$installer" --prefix "$work/mal-absolute" --yes --version "$mal_tag")
assert_contains "$out" "unsafe entry name or link target"
assert_absent "$work/mal-absolute/bin/tuishou"

section "archive with parent path entry is rejected"
mal_tag="v9.9.5"
mal_archive="tuishou-$mal_tag-x86_64-unknown-linux-musl.tar.gz"
mal_checksum="$mal_archive.sha256"
mal_root="$work/mal-parent/root"
mkdir -p "$mal_root"
printf 'untrusted\n' > "$mal_root/evil"
tar -czf "$FIX_SERVER_ROOT/assets/$mal_archive" -C "$mal_root" \
  --transform='s|^evil|../evil|' evil 2>/dev/null
sha256sum "$FIX_SERVER_ROOT/assets/$mal_archive" |
  awk -v name="$mal_checksum" '{ print $1 "  " name }' > "$FIX_SERVER_ROOT/assets/$mal_checksum"
make_malicious_release "$mal_tag" "$mal_archive" "$mal_checksum"
out=$(expect_exit 5 fixture_installer_env bash "$installer" --prefix "$work/mal-parent" --yes --version "$mal_tag")
assert_contains "$out" "unsafe entry name or link target"
assert_absent "$work/mal-parent/bin/tuishou"

section "archive with duplicate entries is rejected"
mal_tag="v9.9.4"
mal_archive="tuishou-$mal_tag-x86_64-unknown-linux-musl.tar.gz"
mal_checksum="$mal_archive.sha256"
mal_root="$work/mal-duplicate/root"
mkdir -p "$mal_root"
printf 'one\n' > "$mal_root/a"
printf 'two\n' > "$mal_root/b"
tar -czf "$FIX_SERVER_ROOT/assets/$mal_archive" -C "$mal_root" \
  --transform='s|^[ab]$|tuishou|' a b
sha256sum "$FIX_SERVER_ROOT/assets/$mal_archive" |
  awk -v name="$mal_checksum" '{ print $1 "  " name }' > "$FIX_SERVER_ROOT/assets/$mal_checksum"
make_malicious_release "$mal_tag" "$mal_archive" "$mal_checksum"
out=$(expect_exit 5 fixture_installer_env bash "$installer" --prefix "$work/mal-duplicate" --yes --version "$mal_tag")
assert_contains "$out" "exactly one top-level tuishou entry"
assert_absent "$work/mal-duplicate/bin/tuishou"

section "archive listing with too many entries is rejected"
mal_tag="v9.9.3"
mal_archive="tuishou-$mal_tag-x86_64-unknown-linux-musl.tar.gz"
mal_checksum="$mal_archive.sha256"
mal_root="$work/mal-many/root"
mkdir -p "$mal_root"
for number in {1..129}; do printf 'x\n' > "$mal_root/file$number"; done
tar -czf "$FIX_SERVER_ROOT/assets/$mal_archive" -C "$mal_root" file{1..129}
sha256sum "$FIX_SERVER_ROOT/assets/$mal_archive" |
  awk -v name="$mal_checksum" '{ print $1 "  " name }' > "$FIX_SERVER_ROOT/assets/$mal_checksum"
make_malicious_release "$mal_tag" "$mal_archive" "$mal_checksum"
out=$(expect_exit 5 fixture_installer_env bash "$installer" --prefix "$work/mal-many" --yes --version "$mal_tag")
assert_contains "$out" "archive listing is missing, too long or unreadable"
assert_absent "$work/mal-many/bin/tuishou"

section "oversized checksum asset is rejected"
mal_tag="v9.9.2"
mal_checksum="tuishou-$mal_tag-x86_64-unknown-linux-musl.tar.gz.sha256"
head -c 70000 /dev/zero | tr '\000' 'x' > "$FIX_SERVER_ROOT/assets/$mal_checksum"
mal_archive="tuishou-$mal_tag-x86_64-unknown-linux-musl.tar.gz"
cp "$FIX_SERVER_ROOT/assets/$FIX_ARCHIVE_GOOD" "$FIX_SERVER_ROOT/assets/$mal_archive"
make_malicious_release "$mal_tag" "$mal_archive" "$mal_checksum"
out=$(expect_exit 5 fixture_installer_env bash "$installer" --prefix "$work/large-checksum" --yes --version "$mal_tag")
assert_contains "$out" "download failed for $mal_checksum"
assert_absent "$work/large-checksum/bin/tuishou"

section "world-writable prefix is rejected"
world_prefix="$work/world-writable"
mkdir -p "$world_prefix"
chmod 0777 "$world_prefix"
out=$(expect_exit 5 fixture_installer_env bash "$installer" --prefix "$world_prefix" --yes)
assert_contains "$out" "writable by group or others"
assert_absent "$world_prefix/bin/tuishou"

section "symlinked prefix is rejected"
ln -s "$work/symlink-target" "$work/symlink-prefix"
out=$(expect_exit 5 fixture_installer_env bash "$installer" --prefix "$work/symlink-prefix" --yes)
assert_contains "$out" "prefix path contains a symlink"
assert_absent "$work/symlink-prefix/bin/tuishou"

section "existing destination is never executed before verification"
preexec_prefix="$work/preexec"
preexec_marker="$work/preexec-ran"
mkdir -p "$preexec_prefix/bin"
printf '#!/bin/sh\n: > "%s"\n' "$preexec_marker" > "$preexec_prefix/bin/tuishou"
chmod 0755 "$preexec_prefix/bin/tuishou"
out=$(fixture_installer_env bash "$installer" --prefix "$preexec_prefix" --yes --no-components)
assert_contains "$out" "attestation: PASS (tuishou verify"
assert_absent "$preexec_marker"
[[ -x $preexec_prefix/bin/tuishou ]]

section "attestation failure removes the new binary"
# shellcheck disable=SC2030
out=$(export TUISHOU_TEST_ATTESTATION=fail; expect_exit 4 fixture_installer_env bash "$installer" --prefix "$work/bad-attestation" --yes)
assert_contains "$out" "attestation: FAIL"
assert_contains "$out" "attestation verification failed"
assert_absent "$work/bad-attestation/bin/tuishou"

section "unwritable prefix prints the root command"
touch "$work/blocked"
out=$(expect_exit 5 fixture_installer_env bash "$installer" --prefix "$work/blocked/child" --yes)
assert_contains "$out" "is not writable"
assert_contains "$out" "sudo bash"

section "success with components"
prefix="$work/success"
out=$(fixture_installer_env bash "$installer" --prefix "$prefix" --yes --components reiki,lu)
assert_contains "$out" "checksum: PASS"
assert_contains "$out" "bootstrap: only the checksum can be checked before tuishou exists"
assert_contains "$out" "attestation: PASS (tuishou verify"
assert_contains "$out" "tuishou: updated to $FIX_GOOD_TAG"
assert_contains "$out" "reiki: updated - -> $FIX_COMPONENT_TAG"
assert_contains "$out" "lu: updated - -> $FIX_COMPONENT_TAG"
assert_contains "$out" "attestation PASS (tuishou install)"
assert_contains "$out" "tuishou status"
assert_contains "$out" "tuishou top"
assert_contains "$out" "tuishou auto-update install-timer --interval 10m"
[[ -x $prefix/bin/tuishou ]]

section "quickstart proof"
status_out=$(fake_env "$prefix/bin/tuishou" status)
assert_contains "$status_out" "reiki: up to date ($FIX_COMPONENT_TAG)"
assert_contains "$status_out" "tuishou: up to date ($FIX_GOOD_TAG)"

section "rerun is idempotent and keeps a rollback copy"
out=$(fixture_installer_env bash "$installer" --prefix "$prefix" --yes --components reiki,lu)
assert_contains "$out" "tuishou: already up to date ($FIX_GOOD_TAG)"
assert_contains "$out" "reiki: already up to date ($FIX_COMPONENT_TAG)"
assert_contains "$out" "lu: already up to date ($FIX_COMPONENT_TAG)"
[[ -f $prefix/bin/tuishou.prev ]]

section "attestation failure restores the previous binary"
# shellcheck disable=SC2031
out=$(export TUISHOU_TEST_ATTESTATION=fail; expect_exit 4 fixture_installer_env bash "$installer" --prefix "$prefix" --yes --version "$FIX_GOOD_TAG")
assert_contains "$out" "attestation verification failed"
version_out=$(fake_env "$prefix/bin/tuishou" --version)
assert_contains "$version_out" "$FIX_GOOD_TAG"

section "no components"
no_comp="$work/no-components"
out=$(fixture_installer_env bash "$installer" --prefix "$no_comp" --yes --no-components)
assert_contains "$out" "components: none (--no-components)"
assert_absent "$no_comp/bin/.fake-reiki-version"
[[ -x $no_comp/bin/tuishou ]]

printf '\ninstaller tests: %d cases passed\n' "$case_no"
