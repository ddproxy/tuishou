#!/usr/bin/env bash
# Single-script installer for the tuishou family.
# curl -fsSL <raw url>/install.sh | bash -s -- [options]
set -euo pipefail

readonly REPO="ddproxy/tuishou"
readonly TARGET="x86_64-unknown-linux-musl"
readonly DEFAULT_COMPONENTS="reiki,lu"
readonly INSTALLER_URL="https://raw.githubusercontent.com/ddproxy/tuishou/main/install.sh"
readonly EXIT_OK=0
readonly EXIT_USAGE=2
readonly EXIT_UNSUPPORTED=3
readonly EXIT_VERIFY=4
readonly EXIT_INSTALL=5

COMPONENTS=()
RESULTS=()
stage_dir=""
bin_dir=""
dest=""
tag=""
prefix=""
release_json=""
archive_name=""
checksum_name=""
original_args=("$@")

info() {
  printf '%s\n' "$*"
}

style() {
  local code="$1"
  shift
  if [[ -z ${NO_COLOR:-} && -t 1 ]]; then
    printf '\033[%sm%s\033[0m\n' "$code" "$*"
  else
    printf '%s\n' "$*"
  fi
}

pass() {
  style 32 "PASS: $*"
}

warn() {
  style 33 "WARN: $*" >&2
}

fail() {
  style 31 "ERROR: $*" >&2
}

usage() {
  cat <<'USAGE'
tuishou installer

Usage:
  curl -fsSL <raw url>/install.sh | bash -s -- [options]
  bash install.sh [options]

Options:
  --components <list>  Comma-separated components to install (default: reiki,lu).
                       Known components: reiki, lu. tuishou is always installed.
  --version <tag>      tuishou release tag (default: latest stable).
  --prefix <dir>       Install root; binaries go in <dir>/bin.
                       Default: /usr/local as root, otherwise ~/.local.
  --no-components      Install tuishou only.
  --dry-run            Resolve the release and print the plan; make no changes.
  --yes                Replace an existing binary without a prompt.
  --help               Show this help.

Exit codes: 0 ok, 2 usage, 3 unsupported platform, 4 verification failed,
5 install failed. No ANSI colour is emitted when NO_COLOR is set.
USAGE
}

die_usage() {
  fail "$1"
  printf 'Run with --help for usage.\n' >&2
  exit "$EXIT_USAGE"
}

die_unsupported() {
  local platform="$1"
  printf "ERROR: unsupported platform: %s\n" "$platform" >&2
  printf 'This installer supports Linux x86_64 (%s) only.\n' "$TARGET" >&2
  printf 'Release targets that exist: %s, aarch64-unknown-linux-musl, x86_64-apple-darwin, aarch64-apple-darwin.\n' "$TARGET" >&2
  printf 'Install another target by downloading its archive manually; see docs/install.md.\n' >&2
  exit "$EXIT_UNSUPPORTED"
}

die_verify() {
  fail "$1"
  exit "$EXIT_VERIFY"
}

die_install() {
  fail "$1"
  exit "$EXIT_INSTALL"
}

cleanup() {
  if [[ -n $stage_dir && -d $stage_dir ]]; then
    rm -rf -- "$stage_dir"
  fi
}
trap cleanup EXIT

normalize_tag() {
  local value="$1"
  if [[ $value == v* ]]; then
    printf '%s' "$value"
  else
    printf 'v%s' "$value"
  fi
}

rerun_command() {
  local out="" arg quoted=""
  if [[ -f ${BASH_SOURCE[0]:-} ]]; then
    printf -v quoted '%q' "${BASH_SOURCE[0]}"
    out="sudo bash $quoted"
  else
    out="curl -fsSL $INSTALLER_URL | sudo bash -s --"
  fi
  for arg in "${original_args[@]}"; do
    printf -v quoted '%q' "$arg"
    out+=" $quoted"
  done
  printf '%s\n' "$out"
}

fail_prefix() {
  fail "prefix $prefix is not writable by this user"
  printf 'Rerun as root with:\n  ' >&2
  rerun_command >&2
  exit "$EXIT_INSTALL"
}

check_prefix_writable() {
  local ancestor="$prefix"
  if [[ -e $prefix ]]; then
    [[ -d $prefix && -w $prefix ]] || fail_prefix
  else
    while [[ ! -e $ancestor ]]; do
      ancestor=$(dirname "$ancestor")
      if [[ $ancestor == / ]]; then
        break
      fi
    done
    [[ -d $ancestor && -w $ancestor ]] || fail_prefix
  fi
  if [[ -e $bin_dir && ! -d $bin_dir ]]; then
    fail_prefix
  fi
}

confirm_replace() {
  local answer=""
  printf 'Replace existing %s? [y/N] ' "$dest" >&2
  if [[ -r /dev/tty ]]; then
    IFS= read -r answer < /dev/tty || answer=""
  else
    IFS= read -r answer || answer=""
  fi
  [[ $answer == [Yy]* ]]
}

parse_components() {
  local raw="$1" item=""
  COMPONENTS=()
  [[ -n $raw ]] || return 0
  while IFS= read -r item; do
    item=${item//[[:space:]]/}
    [[ -n $item ]] || continue
    case "$item" in
      reiki | lu) COMPONENTS+=("$item") ;;
      *) die_usage "unknown component '$item'; known components: reiki, lu" ;;
    esac
  done < <(printf '%s\n' "$raw" | tr ',' '\n')
}

print_next() {
  printf '\nNext commands:\n'
  printf '  tuishou status\n'
  printf '  tuishou top                         (planned; not in this build)\n'
  printf '  tuishou auto-update install-timer --interval 10m\n'
  printf '      Enable first with [auto_update] enabled = true in tuishou.toml.\n'
}

while (($# > 0)); do
  case "$1" in
    --components)
      (($# >= 2)) || die_usage "--components needs a value"
      components_raw="$2"
      shift 2
      ;;
    --components=*)
      components_raw="${1#*=}"
      shift
      ;;
    --version)
      (($# >= 2)) || die_usage "--version needs a tag"
      version_raw="$2"
      shift 2
      ;;
    --version=*)
      version_raw="${1#*=}"
      shift
      ;;
    --prefix)
      (($# >= 2)) || die_usage "--prefix needs a directory"
      prefix="$2"
      shift 2
      ;;
    --prefix=*)
      prefix="${1#*=}"
      shift
      ;;
    --no-components)
      no_components=1
      shift
      ;;
    --dry-run)
      dry_run=1
      shift
      ;;
    --yes)
      yes=1
      shift
      ;;
    --help | -h)
      usage
      exit "$EXIT_OK"
      ;;
    *)
      die_usage "unknown option '$1'"
      ;;
  esac
done

components_raw="${components_raw:-$DEFAULT_COMPONENTS}"
version_raw="${version_raw:-}"
no_components="${no_components:-0}"
dry_run="${dry_run:-0}"
yes="${yes:-0}"

if [[ $no_components -eq 1 ]]; then
  components_raw=""
fi
parse_components "$components_raw"

os=$(uname -s | tr '[:upper:]' '[:lower:]')
arch="${TUISHOU_FAKE_UNAME:-$(uname -m)}"
case "$arch" in
  x86_64 | amd64) arch="x86_64" ;;
esac
if [[ $os != linux || $arch != x86_64 ]]; then
  die_unsupported "$os/$arch"
fi

if [[ -z $prefix ]]; then
  if [[ $(id -u) -eq 0 ]]; then
    prefix="/usr/local"
  else
    [[ -n ${HOME:-} ]] || die_usage "HOME is not set; pass --prefix"
    prefix="$HOME/.local"
  fi
fi
while [[ ${#prefix} -gt 1 && $prefix == */ ]]; do
  prefix="${prefix%/}"
done
bin_dir="$prefix/bin"
dest="$bin_dir/tuishou"

if [[ -n $version_raw ]]; then
  requested_tag=$(normalize_tag "$version_raw")
else
  requested_tag=""
fi

api_base="${TUISHOU_GITHUB_API_BASE:-https://api.github.com}"
if [[ -n $requested_tag ]]; then
  metadata_url="$api_base/repos/$REPO/releases/tags/$requested_tag"
else
  metadata_url="$api_base/repos/$REPO/releases/latest"
fi

if ! release_json=$(curl -fsSL --retry 3 --connect-timeout 10 "$metadata_url"); then
  die_install "could not fetch release metadata for $REPO"
fi
tag=$(printf '%s\n' "$release_json" | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | awk 'NR == 1 { print; exit }')
if [[ ! $tag =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]]; then
  die_install "release metadata did not contain a valid tag"
fi
if [[ -n $requested_tag && $tag != "$requested_tag" ]]; then
  die_install "release $requested_tag resolved to unexpected tag $tag"
fi

archive_name="tuishou-$tag-$TARGET.tar.gz"
checksum_name="$archive_name.sha256"

if [[ $dry_run -eq 1 ]]; then
  info "dry run: no changes will be made"
  info "platform: $os/$arch ($TARGET)"
  info "tuishou: $tag -> $prefix (binary: $dest)"
  if [[ ${#COMPONENTS[@]} -eq 0 ]]; then
    info "components: none (--no-components or empty list)"
  else
    printf 'components:'
    printf ' %s' "${COMPONENTS[@]}"
    printf '\n'
  fi
  info "checksum and attestation: not run in dry run"
  print_next
  exit "$EXIT_OK"
fi

check_prefix_writable

if [[ -e $dest && $yes -eq 0 ]] && ! confirm_replace; then
  die_install "install cancelled; pass --yes to replace without a prompt"
fi

mkdir -p -- "$prefix" "$bin_dir" || fail_prefix
stage_dir=$(mktemp -d "$prefix/.tuishou-install.XXXXXX") || fail_prefix

asset_url() {
  local wanted="$1"
  printf '%s\n' "$release_json" | awk -v want="$wanted" '
    /"name"[[:space:]]*:/ { name = $0 }
    /"browser_download_url"[[:space:]]*:/ {
      if (index(name, "\"" want "\"") > 0) {
        url = $0
        sub(/^.*"browser_download_url"[[:space:]]*:[[:space:]]*"/, "", url)
        sub(/".*$/, "", url)
        print url
        exit
      }
    }
  '
}

check_asset_url() {
  local url="$1"
  if [[ $url != https://* && $url != http://127.0.0.1:* && $url != http://localhost:* ]]; then
    die_install "refusing asset URL with unsupported scheme or host"
  fi
}

archive_url=$(asset_url "$archive_name")
checksum_url=$(asset_url "$checksum_name")
[[ -n $archive_url && -n $checksum_url ]] || die_install "release $tag is missing $archive_name or $checksum_name"
check_asset_url "$archive_url"
check_asset_url "$checksum_url"

if ! curl -fsSL --retry 3 --connect-timeout 10 -o "$stage_dir/$archive_name" "$archive_url"; then
  die_install "download failed for $archive_name"
fi
if ! curl -fsSL --retry 3 --connect-timeout 10 -o "$stage_dir/$checksum_name" "$checksum_url"; then
  die_install "download failed for $checksum_name"
fi

expected=$(awk 'NF { print $1; exit }' "$stage_dir/$checksum_name" | tr '[:upper:]' '[:lower:]')
actual=$(sha256sum "$stage_dir/$archive_name" | awk '{ print $1 }')
if [[ ! $expected =~ ^[0-9a-f]{64}$ ]]; then
  die_verify "checksum file for $archive_name is not a SHA-256 digest"
fi
if [[ $actual != "$expected" ]]; then
  die_verify "checksum mismatch for $archive_name: expected $expected, got $actual"
fi
pass "checksum: PASS ($archive_name)"
info "bootstrap: only the checksum can be checked before tuishou exists; attestation follows installation"

mkdir -p "$stage_dir/extract"
if ! tar -tzf "$stage_dir/$archive_name" | grep -qx 'tuishou'; then
  die_install "archive $archive_name does not contain a top-level tuishou binary"
fi
if ! tar -xzf "$stage_dir/$archive_name" -C "$stage_dir/extract" tuishou; then
  die_install "could not extract tuishou from $archive_name"
fi
chmod 0755 "$stage_dir/extract/tuishou"

previous_tag=""
if [[ -x $dest ]]; then
  previous_tag=$("$dest" --version 2>/dev/null || true)
fi

staged="$bin_dir/.tuishou-commit.$$.tmp"
backup="$bin_dir/.tuishou-prev.$$.tmp"
cp "$stage_dir/extract/tuishou" "$staged" || die_install "could not stage the new binary"
chmod 0755 "$staged"
had_previous=0
if [[ -e $dest ]]; then
  cp "$dest" "$backup" || die_install "could not back up the previous binary"
  chmod 0755 "$backup"
  mv -f "$backup" "$bin_dir/tuishou.prev" || die_install "could not keep the rollback copy"
  had_previous=1
fi
mv -f "$staged" "$dest" || die_install "could not install $dest"

if ! verify_out=$("$dest" verify "$stage_dir/$archive_name" --repo "$REPO" --tag "$tag" 2>&1); then
  warn "attestation: FAIL (tuishou verify $archive_name)"
  printf '%s\n' "$verify_out" >&2
  if [[ $had_previous -eq 1 && -f $bin_dir/tuishou.prev ]]; then
    mv -f "$bin_dir/tuishou.prev" "$dest"
  else
    rm -f "$dest"
  fi
  rm -f "$staged"
  die_verify "attestation verification failed; the new binary was removed"
fi
pass "attestation: PASS (tuishou verify $archive_name)"
printf '%s\n' "$verify_out"

if ! "$dest" adopt tuishou --version "$tag" --prefix "$bin_dir" >/dev/null; then
  die_install "could not record tuishou $tag in state"
fi

if [[ -n $previous_tag && ( $previous_tag == *"$tag"* || $previous_tag == *"${tag#v}"* ) ]]; then
  tuishou_state="already up to date ($tag)"
else
  tuishou_state="updated to $tag"
fi
RESULTS+=("tuishou: $tuishou_state | $bin_dir | checksum PASS, attestation PASS (tuishou verify)")

if [[ ${#COMPONENTS[@]} -gt 0 ]]; then
  for component in "${COMPONENTS[@]}"; do
    if ! component_out=$("$dest" install "$component" --prefix "$bin_dir" 2>&1); then
      printf '%s\n' "$component_out" >&2
      die_install "tuishou install $component failed"
    fi
    line=$(printf '%s\n' "$component_out" | awk -v name="$component" 'index($0, name ": ") == 1 { print; exit }')
    [[ -n $line ]] || die_install "tuishou install $component produced no report"
    if [[ $line == *"up to date ("* && $line != *"already up to date"* ]]; then
      line=${line/up to date (/already up to date (}
    fi
    RESULTS+=("$line | $bin_dir | attestation PASS (tuishou install)")
  done
else
  RESULTS+=("components: none (--no-components)")
fi

rm -rf -- "$stage_dir"
stage_dir=""

printf '\nInstall summary\n'
for line in "${RESULTS[@]}"; do
  printf '  %s\n' "$line"
done
print_next
exit "$EXIT_OK"
