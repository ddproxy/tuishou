#!/usr/bin/env bash
# Fixture binary shipped in fake release archives. It implements only the
# commands the installer and the documented quickstart use.
set -euo pipefail

version="${FAKE_TUISHOU_VERSION:-v9.9.9}"
component_version="${FAKE_COMPONENT_VERSION:-v1.2.3}"
script_dir=$(cd "$(dirname "$0")" && pwd)
command="${1:-}"
if (($# > 0)); then
  shift
fi

case "$command" in
  --version)
    printf 'tuishou %s\n' "$version"
    ;;
  verify)
    artifact="${1:-}"
    if [[ -z $artifact || ! -s $artifact ]]; then
      printf 'attestation refused: missing artifact\n' >&2
      exit 1
    fi
    if [[ ${TUISHOU_TEST_ATTESTATION:-pass} != pass ]]; then
      printf 'attestation refused: fixture failure\n' >&2
      exit 1
    fi
    printf 'digest: fixture-digest\n'
    printf 'issuer: fixture\n'
    printf 'identity: fixture\n'
    printf 'workflow: .github/workflows/release.yml\n'
    printf 'ref: refs/tags/%s\n' "$version"
    printf 'log index: 0\n'
    ;;
  adopt)
    printf 'adopted %s %s\n' "${1:-}" "${FAKE_TUISHOU_VERSION:-v9.9.9}"
    ;;
  install)
    component="${1:-}"
    marker="$script_dir/.fake-$component-version"
    if [[ -f $marker ]] && IFS= read -r old < "$marker" && [[ $old == "$component_version" ]]; then
      printf '%s: already up to date (%s)\n' "$component" "$component_version"
    else
      printf '%s\n' "$component_version" > "$marker"
      printf '%s: updated - -> %s\n' "$component" "$component_version"
    fi
    ;;
  status)
    for component in reiki lu tuishou; do
      if [[ $component == tuishou ]]; then
        installed="$version"
      else
        installed="-"
        marker="$script_dir/.fake-$component-version"
        if [[ -f $marker ]]; then
          IFS= read -r installed < "$marker" || installed="-"
        fi
      fi
      printf '%s: up to date (%s) | auto_update=false installed=%s latest=%s last_check=never last_result=never bad=-\n' \
        "$component" "$installed" "$installed" "$installed"
    done
    ;;
  rollback)
    printf 'rolled back %s\n' "${1:-}"
    ;;
  auto-update)
    printf 'auto_update: off (fixture)\n'
    ;;
  top)
    printf 'unknown subcommand: top\n' >&2
    exit 2
    ;;
  *)
    printf 'unknown subcommand: %s\n' "$command" >&2
    exit 2
    ;;
esac
