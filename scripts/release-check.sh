#!/usr/bin/env bash
# Local release rehearsal:
#   build the release archive layout for the host target,
#   serve it as a fake GitHub release over loopback HTTP,
#   run `tuishou install` against it, and execute the installed binary.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)"
if [ -z "$version" ]; then
  echo "could not read package version from Cargo.toml" >&2
  exit 1
fi
tag="${TUISHOU_REHEARSAL_TAG:-v${version}}"

host="$(rustc -vV | sed -n 's/^host: //p')"
case "$host" in
  x86_64-unknown-linux-*) target="x86_64-unknown-linux-musl" ;;
  aarch64-unknown-linux-*) target="aarch64-unknown-linux-musl" ;;
  x86_64-apple-darwin|aarch64-apple-darwin) target="$host" ;;
  *)
    echo "unsupported host target for release rehearsal: $host" >&2
    exit 1
    ;;
esac
target="${TUISHOU_REHEARSAL_TARGET:-$target}"

echo "release-check: tag=$tag target=$target"

cargo build --release --locked
cargo build --example file_release_server --locked

release_bin="$root/target/release/tuishou"
server_bin="$root/target/debug/examples/file_release_server"
asset="tuishou-${tag}-${target}.tar.gz"
checksum="${asset}.sha256"

tmp="$(mktemp -d)"
dist="$tmp/dist"
server_pid=""
cleanup() {
  if [ -n "$server_pid" ]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  rm -rf "$tmp"
}
trap cleanup EXIT

# build.rs materialises these files under OUT_DIR, never in the source tree.
docs="$(find "$root/target/release/build" -type f -name tuishou.bash -path '*/out/*' -print -quit)"
if [ -z "$docs" ]; then
  echo "release-check: generated docs not found under OUT_DIR" >&2
  exit 1
fi
generated="$(dirname "$docs")"
mkdir -p "$dist"
cp "$release_bin" "$dist/tuishou"
cp "$generated/tuishou.bash" "$generated/tuishou.zsh" "$generated/tuishou.fish" "$generated/tuishou.1" "$dist/"
tar -C "$dist" -czf "$dist/$asset" tuishou tuishou.bash tuishou.zsh tuishou.fish tuishou.1
(cd "$dist" && sha256sum "$asset" > "$checksum")
(cd "$dist" && sha256sum -c "$checksum")

"$server_bin" "$dist" "$tag" > "$tmp/port" 2> "$tmp/server.log" &
server_pid=$!
port=""
for _ in $(seq 1 100); do
  if [ -s "$tmp/port" ]; then
    port="$(head -n 1 "$tmp/port")"
    break
  fi
  sleep 0.1
done
if [ -z "$port" ]; then
  echo "fake release server did not start" >&2
  cat "$tmp/server.log" >&2
  exit 1
fi

home="$tmp/home"
install_dir="$tmp/install"
mkdir -p "$home"

echo "release-check: installing from http://127.0.0.1:$port"
env -u GITHUB_TOKEN -u GH_TOKEN \
  HOME="$home" \
  XDG_STATE_HOME="$home/.local/state" \
  TUISHOU_GITHUB_API_BASE="http://127.0.0.1:$port" \
  TUISHOU_ALLOW_INSECURE_HTTP=1 \
  "$release_bin" install tuishou --version "$tag" --prefix "$install_dir" --allow-unattested

installed="$install_dir/tuishou"
if [ ! -x "$installed" ]; then
  echo "release-check: installed binary missing or not executable" >&2
  exit 1
fi
installed_version="$("$installed" --version)"
expected_version="tuishou ${tag#v}"
if [[ "$installed_version" != *"$expected_version"* ]]; then
  echo "release-check: unexpected version from installed binary: $installed_version" >&2
  exit 1
fi

echo "release-check: installed binary reports: $installed_version"
echo "release-check: ok ($asset, $checksum)"
