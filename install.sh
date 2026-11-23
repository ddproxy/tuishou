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

# Bounds for the pre-verifier bootstrap path. The binary-side caps in
# src/http.rs do not apply to this shell installer.
readonly MAX_METADATA_BYTES=1048576
readonly MAX_ARCHIVE_BYTES=209715200
readonly MAX_CHECKSUM_BYTES=65536
readonly MAX_TAR_ENTRIES=128
readonly MAX_TAR_ENTRY_BYTES=209715200

COMPONENTS=()
RESULTS=()
stage_dir=""
bin_dir=""
dest=""
tag=""
prefix=""
prefix_explicit=0
release_json=""
archive_name=""
checksum_name=""
expect_sha256=""
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
  --expect-sha256 <digest>
                       Pin the archive SHA-256 (64 hex chars) from an
                       independent trusted channel; checked before extraction.
                       The release .sha256 stays a consistency check.
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

# shellcheck disable=SC2329 # invoked through the EXIT trap
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
  if (($# > 0)); then
    fail "$1"
  else
    fail "prefix $prefix is not writable by this user"
  fi
  printf 'Rerun as root with:\n  ' >&2
  rerun_command >&2
  exit "$EXIT_INSTALL"
}

check_no_symlink_components() {
  local target="$1" path="" part
  local -a parts=()
  IFS='/' read -ra parts <<<"$target"
  for part in "${parts[@]}"; do
    [[ -n $part ]] || continue
    if [[ $target == /* && -z $path ]]; then
      path="/$part"
    else
      path="${path:+$path/}$part"
    fi
    [[ -L $path ]] && fail_prefix "prefix path contains a symlink: $path"
  done
  return 0
}

check_dir_trust() {
  local dir="$1" mode owner uid
  [[ -L $dir ]] && fail_prefix "prefix path contains a symlink: $dir"
  if [[ -e $dir ]]; then
    [[ -d $dir ]] || fail_prefix "prefix path is not a directory: $dir"
    mode=$(stat -c '%a' -- "$dir") || fail_prefix "cannot inspect $dir"
    owner=$(stat -c '%u' -- "$dir") || fail_prefix "cannot inspect $dir"
    uid=$(id -u)
    if (( (8#$mode & 8#022) != 0 )); then
      fail_prefix "directory $dir is writable by group or others"
    fi
    if (( uid == 0 )); then
      (( owner == 0 )) || fail_prefix "directory $dir must be owned by root for a privileged install"
    else
      (( owner == uid || owner == 0 )) || fail_prefix "directory $dir is owned by another user"
    fi
  fi
  return 0
}

check_prefix_writable() {
  local ancestor="$prefix"
  check_no_symlink_components "$prefix"
  check_dir_trust "$prefix"
  check_dir_trust "$bin_dir"
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
  if [[ -L $dest ]]; then
    fail_prefix "existing $dest is a symlink"
  fi
  if [[ -e $dest && ! -f $dest ]]; then
    fail_prefix "existing $dest is not a regular file"
  fi
}

installed_state_path() {
  if [[ $prefix_explicit -eq 1 ]]; then
    printf '%s\n' "$bin_dir/.tuishou.json"
  elif [[ -n ${XDG_STATE_HOME:-} ]]; then
    printf '%s\n' "$XDG_STATE_HOME/tuishou/installed.json"
  elif [[ -n ${HOME:-} ]]; then
    printf '%s\n' "$HOME/.local/state/tuishou/installed.json"
  else
    printf '%s\n' ".tuishou.json"
  fi
}

# The previous version comes from local state, never from executing an
# unverified binary that might already sit in the destination directory.
installed_version() {
  local state_file
  state_file=$(installed_state_path)
  if [[ -f $state_file && ! -L $state_file ]]; then
    awk '
      {
        line = $0
        if (!tuishou) {
          if (!match(line, /"tuishou"[[:space:]]*:/)) next
          tuishou = 1
          line = substr(line, RSTART)
        }
        if (match(line, /"version"[[:space:]]*:/)) {
          value = substr(line, RSTART + RLENGTH)
          sub(/^[[:space:]]*"/, "", value)
          sub(/".*$/, "", value)
          print value
          exit
        }
        if (index(line, "}") > 0) tuishou = 0
      }
    ' "$state_file"
  fi
}

download_file() {
  local url="$1" out="$2" limit="$3" label="$4" size
  if ! curl -fsSL --retry 3 --connect-timeout 10 --max-time 300 \
    --max-filesize "$limit" -o "$out" "$url"; then
    die_install "download failed for $label"
  fi
  size=$(wc -c < "$out") || die_install "cannot measure $label"
  if (( size == 0 || size > limit )); then
    die_install "download size out of bounds for $label"
  fi
}

# Accept exactly one ordinary file named tuishou. Any link, device, absolute
# path, parent path or duplicate entry is rejected before extraction.
validate_archive() {
  local archive="$1" listing="$2" line perms size name extra count
  if ! tar -tvzf "$archive" 2>/dev/null |
    awk -v max="$MAX_TAR_ENTRIES" '
      length($0) > 4096 { exit 1 }
      NR <= max { print }
      END { if (NR > max) exit 1 }
    ' > "$listing"; then
    die_install "archive listing is missing, too long or unreadable"
  fi
  count=$(wc -l < "$listing") || die_install "cannot read archive listing"
  if (( count != 1 )); then
    die_install "archive must contain exactly one top-level tuishou entry"
  fi
  IFS= read -r line < "$listing"
  read -r perms _ size _ _ name extra <<<"$line"
  if [[ ${perms:0:1} != '-' ]]; then
    die_install "archive entry tuishou must be a regular file"
  fi
  if [[ $name != tuishou || -n ${extra:-} ]]; then
    die_install "archive contains an unsafe entry name or link target"
  fi
  [[ $size =~ ^[0-9]+$ ]] || die_install "archive entry has no valid size"
  if (( size > MAX_TAR_ENTRY_BYTES )); then
    die_install "archive entry tuishou exceeds the size limit"
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
  printf '  tuishou top                         (host reiki top and lu top)\n'
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
    --expect-sha256)
      (($# >= 2)) || die_usage "--expect-sha256 needs a digest"
      expect_sha256_raw="$2"
      expect_sha256_set=1
      shift 2
      ;;
    --expect-sha256=*)
      expect_sha256_raw="${1#*=}"
      expect_sha256_set=1
      shift
      ;;
    --prefix)
      (($# >= 2)) || die_usage "--prefix needs a directory"
      prefix="$2"
      prefix_explicit=1
      shift 2
      ;;
    --prefix=*)
      prefix="${1#*=}"
      prefix_explicit=1
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
expect_sha256_raw="${expect_sha256_raw:-}"
expect_sha256_set="${expect_sha256_set:-0}"
no_components="${no_components:-0}"
dry_run="${dry_run:-0}"
yes="${yes:-0}"

if [[ $expect_sha256_set -eq 1 ]]; then
  expect_sha256=$(printf '%s' "$expect_sha256_raw" | tr '[:upper:]' '[:lower:]')
  [[ $expect_sha256 =~ ^[0-9a-f]{64}$ ]] ||
    die_verify "--expect-sha256 must be 64 hexadecimal characters (SHA-256)"
fi

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
# Without --prefix, tuishou itself picks its default bin directory and state file; passing the
# directory would put state beside the binaries where a later plain `tuishou status` does not look.
prefix_args=()
if [[ $prefix_explicit -eq 1 ]]; then prefix_args=(--prefix "$bin_dir"); fi
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

if ! release_json=$(curl -fsSL --retry 3 --connect-timeout 10 --max-time 60 \
    --max-filesize "$MAX_METADATA_BYTES" "$metadata_url"); then
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
  if [[ -n $expect_sha256 ]]; then
    info "pinned SHA-256: $expect_sha256 (out of band; checked before extraction)"
  else
    info "pinned SHA-256: none supplied"
  fi
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

# Read the previous version from local state. Executing an existing $dest here
# would run unverified code before any checksum or attestation check.
previous_tag=$(installed_version)

if [[ -e $dest && $yes -eq 0 ]] && ! confirm_replace; then
  die_install "install cancelled; pass --yes to replace without a prompt"
fi

mkdir -p -- "$prefix" "$bin_dir" || fail_prefix
# Directory trust can change between the first check and mkdir.
check_no_symlink_components "$prefix"
check_dir_trust "$prefix"
check_dir_trust "$bin_dir"
if [[ -L $dest ]]; then
  fail_prefix "existing $dest is a symlink"
fi
if [[ -e $dest && ! -f $dest ]]; then
  fail_prefix "existing $dest is not a regular file"
fi
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

download_file "$archive_url" "$stage_dir/$archive_name" "$MAX_ARCHIVE_BYTES" "$archive_name"
actual=$(sha256sum "$stage_dir/$archive_name" | awk '{ print $1 }')
if [[ -n $expect_sha256 && $actual != "$expect_sha256" ]]; then
  die_verify "pinned SHA-256 mismatch for $archive_name: expected $expect_sha256, got $actual"
fi
download_file "$checksum_url" "$stage_dir/$checksum_name" "$MAX_CHECKSUM_BYTES" "$checksum_name"

expected=$(awk 'NF { print $1; exit }' "$stage_dir/$checksum_name" | tr '[:upper:]' '[:lower:]')
if [[ ! $expected =~ ^[0-9a-f]{64}$ ]]; then
  die_verify "checksum file for $archive_name is not a SHA-256 digest"
fi
if [[ $actual != "$expected" ]]; then
  die_verify "checksum mismatch for $archive_name: expected $expected, got $actual"
fi
pass "checksum: PASS ($archive_name)"
if [[ -n $expect_sha256 ]]; then
  pass "pinned SHA-256: PASS ($archive_name)"
  info "bootstrap: the pinned digest came from an independent trusted channel"
else
  warn "bootstrap: no --expect-sha256 supplied; a checksum from the same release is not a trust bootstrap"
fi
info "bootstrap: only the checksum can be checked before tuishou exists; in-process attestation cannot verify the verifier"

validate_archive "$stage_dir/$archive_name" "$stage_dir/archive.listing"
mkdir -p "$stage_dir/extract"
if ! tar -xzf "$stage_dir/$archive_name" -C "$stage_dir/extract" tuishou; then
  die_install "could not extract tuishou from $archive_name"
fi
if [[ -L $stage_dir/extract/tuishou || ! -f $stage_dir/extract/tuishou ]]; then
  die_install "staged tuishou is not a regular file"
fi
chmod 0755 "$stage_dir/extract/tuishou"

# Run the downloaded verifier from private staging. Nothing at $dest is
# executed or replaced until its attestation check succeeds.
if ! verify_out=$("$stage_dir/extract/tuishou" verify "$stage_dir/$archive_name" --repo "$REPO" --tag "$tag" 2>&1); then
  warn "attestation: FAIL (tuishou verify $archive_name)"
  printf '%s\n' "$verify_out" >&2
  die_verify "attestation verification failed; no binary was installed"
fi
pass "attestation: PASS (tuishou verify $archive_name)"
printf '%s\n' "$verify_out"

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
if ! mv -f "$staged" "$dest"; then
  rm -f "$staged"
  if [[ $had_previous -eq 1 && -f $bin_dir/tuishou.prev ]]; then
    mv -f "$bin_dir/tuishou.prev" "$dest" || die_install "could not restore the previous binary"
  fi
  die_install "could not install $dest"
fi

if ! "$dest" adopt tuishou --version "$tag" "${prefix_args[@]}" >/dev/null; then
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
    if ! component_out=$("$dest" install "$component" "${prefix_args[@]}" 2>&1); then
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
