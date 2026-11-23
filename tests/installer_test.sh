#!/usr/bin/env bash
# Installer tests against a local fake GitHub release server.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
installer="$root/install.sh"
work=$(mktemp -d)

# shellcheck source=fixtures/installer/fake_release_server.sh
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

section "attestation failure removes the new binary"
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
