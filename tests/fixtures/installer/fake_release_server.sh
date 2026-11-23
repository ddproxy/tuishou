#!/usr/bin/env bash
# Shared fake GitHub release server for the installer and docs checks.
# It serves a static directory with either python3 or an equivalent shell
# function backed by perl when python3 is unavailable.

FIX_ROOT=""
FIX_REPO="ddproxy/tuishou"
FIX_SERVER_ROOT=""
FIX_PORT=""
FIX_API_BASE=""
FIX_SERVER_PID=""
FIX_FAKE_BIN="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/fake-tuishou.sh"
FIX_GOOD_TAG="v9.9.9"
FIX_BAD_TAG="v9.9.8"
FIX_COMPONENT_TAG="v1.2.3"
FIX_ARCHIVE_GOOD=""
FIX_CHECKSUM_GOOD=""
FIX_ARCHIVE_BAD=""
FIX_CHECKSUM_BAD=""

fixture_free_port() {
  perl -MIO::Socket::INET -e 'my $s = IO::Socket::INET->new(LocalAddr => "127.0.0.1", LocalPort => 0, Proto => "tcp", Listen => 1) or die $!; print $s->sockport;'
}

fixture_asset_names() {
  FIX_ARCHIVE_GOOD="tuishou-$FIX_GOOD_TAG-x86_64-unknown-linux-musl.tar.gz"
  FIX_CHECKSUM_GOOD="$FIX_ARCHIVE_GOOD.sha256"
  FIX_ARCHIVE_BAD="tuishou-$FIX_BAD_TAG-x86_64-unknown-linux-musl.tar.gz"
  FIX_CHECKSUM_BAD="$FIX_ARCHIVE_BAD.sha256"
}

fixture_make_archive() {
  local tag="$1" archive="$2" checksum="$3" root=""
  root="$FIX_ROOT/archive-$tag"
  mkdir -p "$root"
  cp "$FIX_FAKE_BIN" "$root/tuishou"
  chmod 0755 "$root/tuishou"
  tar -czf "$FIX_SERVER_ROOT/assets/$archive" -C "$root" tuishou
  if [[ $tag == "$FIX_BAD_TAG" ]]; then
    printf '%064d  %s\n' 0 "$archive" > "$FIX_SERVER_ROOT/assets/$checksum"
  else
    sha256sum "$FIX_SERVER_ROOT/assets/$archive" | awk -v name="$archive" '{ print $1 "  " name }' > "$FIX_SERVER_ROOT/assets/$checksum"
  fi
}

fixture_write_release() {
  local path="$1" tag="$2" archive="$3" checksum="$4"
  mkdir -p "$(dirname "$path")"
  cat > "$path" <<EOF
{"tag_name":"$tag","prerelease":false,"draft":false,"assets":[
{"name":"$archive","browser_download_url":"$FIX_API_BASE/assets/$archive"},
{"name":"$checksum","browser_download_url":"$FIX_API_BASE/assets/$checksum"}
]}
EOF
}

fixture_start_server() {
  if command -v python3 >/dev/null 2>&1; then
    python3 -m http.server "$FIX_PORT" --bind 127.0.0.1 --directory "$FIX_SERVER_ROOT" \
      > "$FIX_ROOT/server.out" 2>&1 &
  else
    # shellcheck disable=SC2016 # Perl variables are expanded by perl, not bash.
    perl -MIO::Socket::INET -e '
use strict;
use warnings;
my ($dir, $port) = @ARGV;
my $server = IO::Socket::INET->new(
    LocalAddr => "127.0.0.1", LocalPort => $port, Proto => "tcp",
    Listen => 16, ReuseAddr => 1) or die $!;
while (my $client = $server->accept) {
    my $request = <$client> // "";
    while (my $header = <$client>) {
        last if $header =~ /^\r?\n$/;
    }
    my ($path) = $request =~ /^GET\s+(\S+)/;
    $path = "/" unless defined $path;
    $path =~ s/\?.*//;
    my $relative = $path;
    $relative =~ s{^/}{};
    $relative = "index.html" if $relative eq "";
    my $safe = 1;
    for my $part (split m{/}, $relative) {
        $safe = 0 if $part eq ".." || $part eq ".";
    }
    my $file = $dir . "/" . $relative;
    if ($safe && -f $file) {
        open my $handle, "<:raw", $file or die $!;
        my $body = do { local $/; <$handle> };
        close $handle;
        print $client "HTTP/1.1 200 OK\r\nContent-Length: " . length($body)
            . "\r\nConnection: close\r\n\r\n" . $body;
    } else {
        print $client "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    }
    close $client;
}
' "$FIX_SERVER_ROOT" "$FIX_PORT" > "$FIX_ROOT/server.out" 2>&1 &
  fi
  FIX_SERVER_PID=$!

  for _ in {1..50}; do
    if curl -sS -o /dev/null "$FIX_API_BASE/" > /dev/null 2>&1; then
      return 0
    fi
    sleep 0.1
  done
  printf 'fake release server did not start on %s\n' "$FIX_PORT" >&2
  return 1
}

fixture_stop_server() {
  if [[ -n $FIX_SERVER_PID ]]; then
    kill "$FIX_SERVER_PID" 2> /dev/null || true
    wait "$FIX_SERVER_PID" 2> /dev/null || true
    FIX_SERVER_PID=""
  fi
}

fixture_init() {
  FIX_ROOT="$1"
  FIX_SERVER_ROOT="$FIX_ROOT/server"
  FIX_PORT=$(fixture_free_port)
  FIX_API_BASE="http://127.0.0.1:$FIX_PORT"
  mkdir -p "$FIX_SERVER_ROOT/assets" "$FIX_SERVER_ROOT/repos/$FIX_REPO/releases"
  fixture_asset_names
  fixture_make_archive "$FIX_GOOD_TAG" "$FIX_ARCHIVE_GOOD" "$FIX_CHECKSUM_GOOD"
  fixture_make_archive "$FIX_BAD_TAG" "$FIX_ARCHIVE_BAD" "$FIX_CHECKSUM_BAD"
  fixture_write_release "$FIX_SERVER_ROOT/repos/$FIX_REPO/releases/latest" \
    "$FIX_GOOD_TAG" "$FIX_ARCHIVE_GOOD" "$FIX_CHECKSUM_GOOD"
  fixture_write_release "$FIX_SERVER_ROOT/repos/$FIX_REPO/releases/tags/$FIX_GOOD_TAG" \
    "$FIX_GOOD_TAG" "$FIX_ARCHIVE_GOOD" "$FIX_CHECKSUM_GOOD"
  fixture_write_release "$FIX_SERVER_ROOT/repos/$FIX_REPO/releases/tags/$FIX_BAD_TAG" \
    "$FIX_BAD_TAG" "$FIX_ARCHIVE_BAD" "$FIX_CHECKSUM_BAD"
  fixture_start_server
}

fixture_installer_env() {
  env TUISHOU_GITHUB_API_BASE="$FIX_API_BASE" \
    FAKE_TUISHOU_VERSION="$FIX_GOOD_TAG" \
    FAKE_COMPONENT_VERSION="$FIX_COMPONENT_TAG" \
    NO_COLOR=1 \
    "$@"
}

fake_env() {
  env FAKE_TUISHOU_VERSION="$FIX_GOOD_TAG" \
    FAKE_COMPONENT_VERSION="$FIX_COMPONENT_TAG" \
    "$@"
}
