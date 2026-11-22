#!/usr/bin/env bash
# Fixture tests for scripts/validate-workflows.sh.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
validator="$root/scripts/validate-workflows.sh"
valid="$root/scripts/fixtures/workflows-valid"
invalid="$root/scripts/fixtures/workflows-invalid"

if ! bash "$validator" "$valid"; then
  echo "validator rejected the valid fixtures" >&2
  exit 1
fi

shopt -s nullglob
fixtures=("$invalid"/*.yml)
if [ "${#fixtures[@]}" -eq 0 ]; then
  echo "no negative fixtures found in $invalid" >&2
  exit 1
fi

for f in "${fixtures[@]}"; do
  if bash "$validator" "$f" >/dev/null 2>&1; then
    echo "validator accepted negative fixture: $f" >&2
    exit 1
  fi
  output="$(bash "$validator" "$f" 2>&1 || true)"
  if ! grep -q 'invalid uses: pin' <<<"$output"; then
    echo "negative fixture did not report a pin error: $f" >&2
    printf '%s\n' "$output" >&2
    exit 1
  fi
done

echo "validator fixture tests ok (${#fixtures[@]} negative fixtures)"
