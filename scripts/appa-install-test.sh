#!/bin/sh
# Exercise scripts/appa-install.sh against a local release.
#
# A python3 HTTP server plays GitHub: it serves a staged release tree and
# answers `releases/latest` with the same redirect GitHub sends. Every
# acceptance path of the installer runs against it, on this machine's own
# platform, with a stub `appa` in place of the real binary.
set -eu

repo=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
installer=$repo/scripts/appa-install.sh
tag=v0.0.0-smoke

work=$(mktemp -d)
server_pid=
cleanup() {
  if [ -n "$server_pid" ]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  rm -rf "$work"
}
trap cleanup EXIT

if command -v sha256sum >/dev/null 2>&1; then
  sums() { sha256sum "$@"; }
else
  sums() { shasum -a 256 "$@"; }
fi

# Stage the release: one stub archive per supported target, laid out exactly as
# GitHub serves release assets, plus a second "repository" whose latest
# redirect points at another host.
release=$work/site/good/releases/download/$tag
mkdir -p "$release" "$work/stage"
printf '#!/bin/sh\necho "appa 0.0.0-smoke"\n' > "$work/stage/appa"
chmod 755 "$work/stage/appa"
for target in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu \
  x86_64-apple-darwin aarch64-apple-darwin; do
  tar -C "$work/stage" -czf "$release/appa-$target.tar.gz" .
done
(cd "$release" && sums appa-*.tar.gz > SHA256SUMS)

cat > "$work/server.py" <<'EOF'
import functools
import http.server
import socketserver
import sys

root, port_file, tag = sys.argv[1:4]


class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        origin = f"http://127.0.0.1:{self.server.server_port}"
        match self.path:
            case "/good/releases/latest":
                self.redirect(f"{origin}/good/releases/tag/{tag}")
            case "/foreign/releases/latest":
                self.redirect(f"http://elsewhere.invalid/releases/tag/{tag}")
            case _:
                super().do_GET()

    def redirect(self, location):
        self.send_response(302)
        self.send_header("Location", location)
        self.end_headers()

    def log_message(self, *_args):
        pass


class Server(http.server.HTTPServer):
    # HTTPServer.server_bind resolves the bound host with getfqdn, which can
    # stall for a long time on macOS runners. The name is never used here.
    def server_bind(self):
        socketserver.TCPServer.server_bind(self)
        self.server_name, self.server_port = self.server_address[:2]


server = Server(("127.0.0.1", 0), functools.partial(Handler, directory=root))
with open(port_file, "w") as handle:
    handle.write(str(server.server_port))
server.serve_forever()
EOF
python3 "$work/server.py" "$work/site" "$work/port" "$tag" &
server_pid=$!
for _ in 1 2 3 4 5 6 7 8 9 10; do
  [ -s "$work/port" ] && break
  kill -0 "$server_pid" 2>/dev/null || { echo "release server exited" >&2; exit 1; }
  sleep 1
done
[ -s "$work/port" ] || { echo "release server did not start within 10s" >&2; exit 1; }
origin=http://127.0.0.1:$(cat "$work/port")

failures=0
report() {
  printf '%s: %s\n' "$1" "$2" >&2
  if [ "$1" = FAIL ]; then failures=$((failures + 1)); fi
}

# Each case gets a fresh install directory, with a space in its name.
case_dir() {
  printf '%s\n' "$work/cases/$1/install dir"
}

expect_installed() {
  name=$1
  shift
  dir=$(case_dir "$name")
  if env APPA_REPOSITORY_URL="$origin/good" APPA_INSTALL_DIR="$dir" "$@" \
    sh "$installer" >"$work/$name.out" 2>"$work/$name.err" &&
    [ "$("$dir/appa" --version)" = "appa 0.0.0-smoke" ] &&
    grep -q "Installed appa 0.0.0-smoke" "$work/$name.out"; then
    report PASS "$name"
  else
    cat "$work/$name.out" "$work/$name.err" >&2
    report FAIL "$name"
  fi
}

expect_refused() {
  name=$1
  shift
  dir=$(case_dir "$name")
  if env APPA_INSTALL_DIR="$dir" "$@" sh "$installer" >"$work/$name.out" 2>"$work/$name.err"; then
    report FAIL "$name (installer succeeded)"
  elif [ -e "$dir/appa" ]; then
    report FAIL "$name (installed despite failing)"
  elif ! grep -q '^appa-install: ' "$work/$name.err"; then
    cat "$work/$name.err" >&2
    report FAIL "$name (no installer error message)"
  else
    report PASS "$name"
  fi
}

expect_installed latest-redirect
expect_installed pinned-version APPA_VERSION="$tag"

expect_refused invalid-version APPA_REPOSITORY_URL="$origin/good" APPA_VERSION='../main'
expect_refused foreign-redirect APPA_REPOSITORY_URL="$origin/foreign"
expect_refused relative-install-dir APPA_REPOSITORY_URL="$origin/good" APPA_INSTALL_DIR=relative/bin

# The rest tamper with the release itself, so restore it after each case.
cp "$release/SHA256SUMS" "$work/SHA256SUMS.good"
restore() { cp "$work/SHA256SUMS.good" "$release/SHA256SUMS"; }

grep -Ev 'apple-darwin|linux-gnu' "$work/SHA256SUMS.good" > "$release/SHA256SUMS" || true
expect_refused unlisted-archive APPA_REPOSITORY_URL="$origin/good"
restore

cat "$work/SHA256SUMS.good" "$work/SHA256SUMS.good" > "$release/SHA256SUMS"
expect_refused duplicate-listing APPA_REPOSITORY_URL="$origin/good"
restore

# Rotating every hex digit changes every hash while keeping the names.
while read -r hash name; do
  printf '%s  %s\n' "$(printf '%s' "$hash" | tr '0123456789abcdef' '123456789abcdef0')" "$name"
done < "$work/SHA256SUMS.good" > "$release/SHA256SUMS"
expect_refused digest-mismatch APPA_REPOSITORY_URL="$origin/good"

# A PATH holding everything the installer needs except a digest tool.
mkdir "$work/nodigest"
for tool in sh curl tar gzip mktemp install uname grep cut mkdir rm cat env; do
  ln -s "$(command -v "$tool")" "$work/nodigest/$tool"
done
expect_refused no-digest-tool APPA_REPOSITORY_URL="$origin/good" PATH="$work/nodigest"

if [ "$failures" -ne 0 ]; then
  echo "$failures installer case(s) failed" >&2
  exit 1
fi
echo "all installer cases passed" >&2
