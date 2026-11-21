#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fail=0

check_file() {
  local f="$1"
  if [ ! -f "$f" ]; then
    echo "missing: $f" >&2
    fail=1
    return
  fi
  if ! grep -q '^permissions: {}' "$f"; then
    echo "missing top-level permissions: {} in $f" >&2
    fail=1
  fi
  while IFS= read -r line; do
    # uses: owner/action@sha # vX
    if ! echo "$line" | grep -Eq 'uses: [^@]+@[0-9a-f]{7,40} # .+'; then
      echo "unpinned or missing version comment: $f: $line" >&2
      fail=1
    fi
  done < <(grep -E '^[[:space:]]*uses:' "$f" || true)
}

for f in "$root"/.github/workflows/*.yml "$root"/.github/workflows/*.yaml; do
  [ -e "$f" ] || continue
  check_file "$f"
done

# dependency-review allow-licenses should include deny.toml allows
deny_allows=$(grep -E '"' "$root/deny.toml" | grep -E 'MIT|Apache|BSD|BSL|CDLA|ISC|Unicode|Unlicense|Zlib|NCSA' | sed -E 's/.*"([^"]+)".*/\1/' | tr '\n' ',' | sed 's/,$//')
if ! grep -q 'NCSA' "$root/.github/workflows/dependency-review.yml"; then
  echo "dependency-review missing NCSA" >&2
  fail=1
fi

if [ "$fail" -ne 0 ]; then
  echo "workflow validation failed" >&2
  exit 1
fi
echo "workflow validation ok"
