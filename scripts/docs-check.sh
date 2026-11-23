#!/usr/bin/env bash
# Execute documented quickstart commands and validate docs examples.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
work=$(mktemp -d)
real_curl=$(command -v curl)

# shellcheck source-path=SCRIPTDIR source=../tests/fixtures/installer/fake_release_server.sh
source "$root/tests/fixtures/installer/fake_release_server.sh"

cleanup() {
  fixture_stop_server
  rm -rf -- "$work"
}
trap cleanup EXIT

fixture_init "$work"
mkdir -p "$work/bin"

fail() {
  printf 'docs-check: %s\n' "$*" >&2
  exit 1
}

assert_contains() {
  local output="$1" needle="$2"
  if [[ $output != *"$needle"* ]]; then
    printf 'expected quickstart output to contain: %s\n' "$needle" >&2
    printf '%s\n' "$output" >&2
    return 1
  fi
}

cat > "$work/bin/curl" <<'FAKE_CURL'
#!/usr/bin/env bash
set -euo pipefail
if (($# == 0)); then
  printf 'fake curl: missing URL\n' >&2
  exit 1
fi
url="${!#}"
output=""
remote_name=0
previous=""
for argument in "$@"; do
  case $previous in
    -o | --output) output="$argument" ;;
  esac
  case $argument in
    -o | --output) ;;
    -O | --remote-name) remote_name=1 ;;
  esac
  previous="$argument"
done
if [[ $url == *"raw.githubusercontent.com/ddproxy/tuishou/main/install.sh"* ]]; then
  if [[ -n $output ]]; then
    cat "$DOCS_CHECK_ROOT/install.sh" > "$output"
  elif [[ $remote_name -eq 1 ]]; then
    cat "$DOCS_CHECK_ROOT/install.sh" > install.sh
  else
    cat "$DOCS_CHECK_ROOT/install.sh"
  fi
  exit 0
fi
case $url in
  http://127.0.0.1:* | http://localhost:*)
    exec "$DOCS_CHECK_REAL_CURL" "$@"
    ;;
  *)
    printf 'fake curl: unexpected URL: %s\n' "$url" >&2
    exit 1
    ;;
esac
FAKE_CURL
chmod 0755 "$work/bin/curl"

extract_shell_blocks() {
  local file="$1" heading="${2:-}"
  awk -v heading="$heading" '
    BEGIN { capture = (heading == "") ? 1 : 0; in_block = 0 }
    heading != "" && /^## / {
      if (index($0, "## " heading) == 1) {
        capture = 1
      } else if (capture) {
        capture = 0
      }
    }
    capture && /^[[:space:]]*```sh[[:space:]]*$/ { in_block = 1; next }
    in_block && /^[[:space:]]*```[[:space:]]*$/ { in_block = 0; print ""; next }
    in_block { print }
  ' "$file"
}

run_quickstart() {
  local name="$1" file="$2" heading="$3"
  local run_root="$work/$name" expected_count expected quickstart_output
  mkdir -p "$run_root/home" "$run_root/run" "$run_root/state"
  {
    printf '%s\n' 'set -euo pipefail'
    printf 'cd %q\n' "$run_root/run"
    printf 'export HOME=%q\n' "$run_root/home"
    printf 'export PATH=%q\n' "$work/bin:$PATH"
    printf 'export XDG_STATE_HOME=%q\n' "$run_root/state"
    printf 'export DOCS_CHECK_ROOT=%q DOCS_CHECK_REAL_CURL=%q\n' "$root" "$real_curl"
    printf 'export NO_COLOR=1 TUISHOU_GITHUB_API_BASE=%q\n' "$FIX_API_BASE"
    printf 'export FAKE_TUISHOU_VERSION=%q FAKE_COMPONENT_VERSION=%q\n' \
      "$FIX_GOOD_TAG" "$FIX_COMPONENT_TAG"
    printf 'unset TUISHOU_TEST_ATTESTATION\n'
    extract_shell_blocks "$file" "$heading"
  } > "$run_root/quickstart.sh"

  grep -q 'curl -fsSL' "$run_root/quickstart.sh" ||
    fail "no installer command found in $file"
  expected_count=$(sed -n 's/^[[:space:]]*# expect:[[:space:]]*//p' \
    "$run_root/quickstart.sh" | wc -l)
  ((expected_count > 0)) || fail "no expect markers found in $file"

  if ! bash "$run_root/quickstart.sh" > "$run_root/quickstart.out" 2>&1; then
    cat "$run_root/quickstart.out" >&2
    fail "quickstart commands failed in $file"
  fi
  cat "$run_root/quickstart.out"
  quickstart_output=$(cat "$run_root/quickstart.out")

  while IFS= read -r expected; do
    [[ -n $expected ]] || continue
    assert_contains "$quickstart_output" "$expected"
  done < <(sed -n 's/^[[:space:]]*# expect:[[:space:]]*//p' "$run_root/quickstart.sh")
}

run_quickstart readme "$root/README.md" Quickstart
run_quickstart guide "$root/docs/quickstart.md" ""
printf '%s\n' 'docs-check: quickstart commands passed'

awk -v out="$work" '
  /^[[:space:]]*```toml[[:space:]]*$/ { in_block = 1; count += 1; file = sprintf("%s/config-%03d.toml", out, count); next }
  in_block && /^[[:space:]]*```[[:space:]]*$/ { in_block = 0; close(file); next }
  in_block { print > file }
' "$root/README.md" "$root"/docs/*.md

mapfile -t configs < <(find "$work" -maxdepth 1 -name 'config-*.toml' -type f | sort)
((${#configs[@]} > 0)) || fail "no toml examples found"
for config in "${configs[@]}"; do
  if ! XDG_STATE_HOME="$work/state" cargo run --quiet --locked -- \
    --config "$config" auto-update install-timer > "$work/config.out" 2>&1; then
    cat "$work/config.out" >&2
    fail "config example did not validate: $config"
  fi
done
printf 'docs-check: validated %d config examples\n' "${#configs[@]}"

check_links() {
  local file directory match link target
  while IFS= read -r file; do
    directory=$(dirname "$file")
    while IFS= read -r match; do
      link=${match#](}
      link=${link%)}
      link=${link%%#*}
      [[ -n $link ]] || continue
      case $link in
        http://* | https://* | mailto:*) continue ;;
      esac
      target="$directory/$link"
      [[ -e $target ]] || fail "broken local link in $file: $link"
    done < <(grep -hoE '\]\([^)]+\)' "$file" || true)
  done < <(
    printf '%s\n' "$root/README.md" "$root/llms.txt"
    for extra in AGENTS.md SECURITY.md CHANGELOG.md; do
      if [[ -f $root/$extra ]]; then
        printf '%s\n' "$root/$extra"
      fi
    done
    find "$root/docs" -maxdepth 1 -type f -name '*.md' | sort
  )
}
check_links
printf '%s\n' 'docs-check: local links resolved'

check_style() {
  local file line
  while IFS= read -r file; do
    [[ -s $file ]] || fail "empty document: $file"
    [[ $(tail -c 1 "$file" | od -An -tuC | tr -d '[:space:]') == 10 ]] ||
      fail "missing final newline: $file"
    if line=$(grep -n '[[:blank:]]$' "$file" | head -n 1); then
      fail "trailing whitespace in $file at line $line"
    fi
    if line=$(grep -n "$(printf '\t')" "$file" | head -n 1); then
      fail "tab in $file at line $line"
    fi
  done < <(
    printf '%s\n' "$root/README.md" "$root/AGENTS.md" "$root/SECURITY.md" \
      "$root/CHANGELOG.md" "$root/llms.txt"
    find "$root/docs" -maxdepth 1 -type f -name '*.md' | sort
  )
  for file in "$root"/docs/*.md; do
    head -n 1 "$file" | grep -q '^# ' || fail "missing title in $file"
  done
}
check_style
printf '%s\n' 'docs-check: style checks passed'

check_drift() {
  local flag command
  while IFS= read -r flag; do
    [[ -n $flag ]] || continue
    grep -qF -- "$flag" "$root/docs/install.md" ||
      fail "installer flag missing from docs/install.md: $flag"
  done < <(sed -n 's/^  \(--[a-z-][a-z-]*\).*/\1/p' "$root/install.sh" | sort -u)
  for flag in --json --quiet --verbose --config; do
    grep -qF -- "$flag" "$root/docs/cli.md" ||
      fail "global flag missing from docs/cli.md: $flag"
  done
  for command in install update status rollback self-update completions man verify \
    adopt auto-update; do
    grep -qF -- "$command" "$root/docs/cli.md" ||
      fail "command missing from docs/cli.md: $command"
  done
  if grep -qF 'tuishou top' "$root/docs/cli.md" &&
    ! grep -qE 'not implemented|planned|reserved|does not exist' "$root/docs/cli.md"; then
    fail 'docs/cli.md must mark tuishou top as unimplemented'
  fi
}
check_drift
printf '%s\n' 'docs-check: flag drift checks passed'

check_secret_literals() {
  local file github_prefix aws_prefix pattern
  github_prefix='ghp_'
  aws_prefix='AKIA'
  pattern="${github_prefix}[A-Za-z0-9]{20,}|${aws_prefix}[A-Za-z0-9]{16,}"
  pattern+='|-----BEGIN [A-Z ]*PRIVATE KEY-----'
  pattern+='|[A-Za-z0-9_-]{32,}\.[A-Za-z0-9_-]{16,}\.[A-Za-z0-9_-]{20,}'
  while IFS= read -r file; do
    if grep -nE "$pattern" "$file"; then
      fail "secret-shaped literal in $file"
    fi
  done < <(
    printf '%s\n' "$root/README.md" "$root/AGENTS.md" "$root/SECURITY.md" \
      "$root/CHANGELOG.md" "$root/llms.txt" "$root/install.sh"
    find "$root/docs" -maxdepth 1 -type f -name '*.md' | sort
    find "$root/scripts" -maxdepth 1 -type f -name '*.sh' | sort
  )
}
check_secret_literals
printf '%s\n' 'docs-check: literal checks passed'

printf '%s\n' 'docs-check: all checks passed'
