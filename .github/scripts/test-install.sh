#!/bin/sh
set -eu

die() {
  printf 'installer test: %s\n' "$*" >&2
  exit 1
}

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
tmp_dir=$(mktemp -d 2>/dev/null || mktemp -d -t appa-installer-test)
cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup 0 1 2 15

case "$(uname -s)" in
  Linux) target_os=unknown-linux-gnu ;;
  Darwin) target_os=apple-darwin ;;
  *) die "test requires Linux or macOS" ;;
esac
case "$(uname -m)" in
  x86_64|amd64) target_arch=x86_64 ;;
  arm64|aarch64) target_arch=aarch64 ;;
  *) die "unsupported test architecture" ;;
esac

version=9.8.7
archive=appa-runtime-v2-$target_arch-$target_os.tar.gz
release_dir=$tmp_dir/release
package_dir=$tmp_dir/package
mkdir -p "$release_dir" "$package_dir/claude-code/.claude-plugin" \
  "$package_dir/claude-code/plugin/.claude-plugin" \
  "$package_dir/claude-code/plugin/hooks" \
  "$package_dir/claude-code/plugin/skills/appa-tool-sync"

cat >"$package_dir/appa-runtime-v2" <<EOF
#!/bin/sh
if [ "\${1:-}" = --version ]; then
  printf 'appa-runtime-v2 $version\\n'
  exit 0
fi
exit 0
EOF
chmod 755 "$package_dir/appa-runtime-v2"
printf '{}\n' >"$package_dir/claude-code/.claude-plugin/marketplace.json"
printf '{}\n' >"$package_dir/claude-code/plugin/.claude-plugin/plugin.json"
printf '{}\n' >"$package_dir/claude-code/plugin/.mcp.json"
printf '{}\n' >"$package_dir/claude-code/plugin/hooks/hooks.json"
printf 'session context\n' >"$package_dir/claude-code/plugin/hooks/session-context.md"
printf 'tool sync\n' >"$package_dir/claude-code/plugin/skills/appa-tool-sync/SKILL.md"
printf '#!/bin/sh\nexit 0\n' >"$package_dir/claude-code/plugin/statusline.sh"
chmod 755 "$package_dir/claude-code/plugin/statusline.sh"
tar -C "$package_dir" -czf "$release_dir/$archive" .
printf '%s\n' "$version" >"$release_dir/version.txt"
if command -v sha256sum >/dev/null 2>&1; then
  checksum=$(sha256sum "$release_dir/$archive")
else
  checksum=$(shasum -a 256 "$release_dir/$archive")
fi
checksum=${checksum%% *}
printf '%s  %s\n' "$checksum" "$archive" >"$release_dir/SHA256SUMS"

real_home=$HOME
export HOME="$tmp_dir/home"
export APPA_INSTALL_DIR="$tmp_dir/install/bin"
export APPA_CONFIG_DIR="$tmp_dir/install/config"
export APPA_DATA_DIR="$tmp_dir/install/data"
export APPA_DOWNLOAD_BASE="file://$release_dir"
export APPA_SKIP_SERVICE=1
mkdir -p "$HOME"

if APPA_INSTALL_DIR=relative sh "$repository_root/install.sh" >"$tmp_dir/relative.stdout" 2>"$tmp_dir/relative.stderr"; then
  die "relative installation directory was accepted"
fi
grep -F "installation directories must be absolute" "$tmp_dir/relative.stderr" >/dev/null ||
  die "relative installation directory did not produce the expected refusal"

if APPA_DOWNLOAD_BASE=http://127.0.0.1:80@example.com \
  sh "$repository_root/install.sh" >"$tmp_dir/url.stdout" 2>"$tmp_dir/url.stderr"; then
  die "non-loopback plaintext URL was accepted"
fi
grep -F "permits HTTP only for an exact loopback host" "$tmp_dir/url.stderr" >/dev/null ||
  die "non-loopback plaintext URL did not produce the expected refusal"

output=$(sh <"$repository_root/install.sh")
printf '%s\n' "$output" | grep -F "Installed appa-runtime-v2 $version." >/dev/null ||
  die "installer did not report installed version"
installed_binary=$APPA_INSTALL_DIR/appa-runtime-v2
installed_plugin=$APPA_DATA_DIR/claude-code
[ -x "$installed_binary" ] || die "runtime was not installed"
[ -f "$installed_plugin/.claude-plugin/marketplace.json" ] || die "marketplace was not installed"
[ -f "$installed_plugin/plugin/.claude-plugin/plugin.json" ] || die "plugin manifest was not installed"
[ -f "$installed_plugin/plugin/.mcp.json" ] || die "MCP configuration was not installed"
[ -f "$installed_plugin/plugin/hooks/hooks.json" ] || die "plugin was not installed"
[ -f "$installed_plugin/plugin/hooks/session-context.md" ] || die "session context was not installed"
[ -f "$installed_plugin/plugin/skills/appa-tool-sync/SKILL.md" ] || die "tool-sync skill was not installed"
[ "$($installed_binary --version)" = "appa-runtime-v2 $version" ] || die "installed version is wrong"

output=$(sh "$repository_root/install.sh")
printf '%s\n' "$output" | grep -F "appa-runtime-v2 $version is already installed." >/dev/null ||
  die "second run did not report the installation as unchanged"
printf '%s\n' "$output" | grep -F "Installed: $version" >/dev/null ||
  die "second run did not report the installed version"
printf '%s\n' "$output" | grep -F "Release: $version" >/dev/null ||
  die "second run did not report the release version"
printf '%s\n' "$output" | grep -F -- "--upgrade" >/dev/null ||
  die "second run did not name the upgrade option"
if printf '%s\n' "$output" | grep -F "Installed appa-runtime-v2" >/dev/null; then
  die "second run reinstalled instead of reporting"
fi

printf '9.8.8\n' >"$release_dir/version.txt"
output=$(sh "$repository_root/install.sh")
printf '%s\n' "$output" | grep -F "A newer appa-runtime-v2 release is available." >/dev/null ||
  die "an older installation was not reported as upgradable"
printf '%s\n' "$output" | grep -F "Release: 9.8.8" >/dev/null ||
  die "upgradable report did not name the release version"

printf '9.8.6\n' >"$release_dir/version.txt"
output=$(sh "$repository_root/install.sh")
printf '%s\n' "$output" | grep -F "The installed appa-runtime-v2 is newer than this release." >/dev/null ||
  die "a newer installation was not reported as newer"
printf '%s\n' "$output" | grep -F "Installed: $version" >/dev/null ||
  die "newer-installation report did not name the installed version"
printf '%s\n' "$version" >"$release_dir/version.txt"

printf 'policy survives update\n' >"$APPA_CONFIG_DIR/appa.toml"
printf 'database survives update\n' >"$APPA_DATA_DIR/appa.db"
sh -s -- --upgrade <"$repository_root/install.sh" >/dev/null
[ "$(cat "$APPA_CONFIG_DIR/appa.toml")" = "policy survives update" ] || die "policy was replaced"
[ "$(cat "$APPA_DATA_DIR/appa.db")" = "database survives update" ] || die "database was replaced"

printf 'tampered\n' >>"$release_dir/$archive"
if sh -s -- --upgrade <"$repository_root/install.sh" >"$tmp_dir/tampered.stdout" 2>"$tmp_dir/tampered.stderr"; then
  die "tampered archive was accepted"
fi
grep -F "checksum mismatch for $archive" "$tmp_dir/tampered.stderr" >/dev/null ||
  die "tampered archive did not fail at checksum verification"
[ "$($installed_binary --version)" = "appa-runtime-v2 $version" ] ||
  die "failed update changed installed runtime"

sh -s -- --uninstall <"$repository_root/install.sh" >/dev/null
[ ! -e "$installed_binary" ] || die "runtime survived uninstall"
[ ! -e "$installed_plugin" ] || die "plugin survived uninstall"
[ "$(cat "$APPA_CONFIG_DIR/appa.toml")" = "policy survives update" ] ||
  die "uninstall removed policy"
[ "$(cat "$APPA_DATA_DIR/appa.db")" = "database survives update" ] ||
  die "uninstall removed database"

# Startup registration. The phases above skip the service so they can run
# anywhere; this one installs the real login service, so it needs the
# platform supervisor and it writes into the tester's own home directory.
service_name=appa-runtime-v2.service
launchd_label=ai.archestra.appa-runtime-v2
export HOME="$real_home"
unit=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/$service_name
plist=$HOME/Library/LaunchAgents/$launchd_label.plist

skip_service_test() {
  printf 'Skipped the startup registration test: %s\n' "$*"
  printf 'Unix installer tests passed.\n'
  exit 0
}

command -v python3 >/dev/null 2>&1 || skip_service_test "python3 is missing"
case "$target_os" in
  unknown-linux-gnu)
    if ! command -v systemctl >/dev/null 2>&1; then
      skip_service_test "systemctl is missing"
    fi
    if ! systemctl --user show-environment >/dev/null 2>&1; then
      skip_service_test "no systemd user manager"
    fi
    [ ! -e "$unit" ] || skip_service_test "$unit already exists"
    ;;
  apple-darwin)
    if ! command -v launchctl >/dev/null 2>&1; then
      skip_service_test "launchctl is missing"
    fi
    if ! launchctl print "gui/$(id -u)" >/dev/null 2>&1; then
      skip_service_test "no launchd GUI domain"
    fi
    [ ! -e "$plist" ] || skip_service_test "$plist already exists"
    ;;
esac
if curl --silent --max-time 1 http://127.0.0.1:8787/health >/dev/null 2>&1; then
  skip_service_test "port 8787 is in use"
fi

service_release=$tmp_dir/service-release
service_package=$tmp_dir/service-package
mkdir -p "$service_release" "$service_package/claude-code/.claude-plugin" \
  "$service_package/claude-code/plugin/.claude-plugin" \
  "$service_package/claude-code/plugin/hooks" \
  "$service_package/claude-code/plugin/skills/appa-tool-sync"
cp -R "$package_dir/claude-code/." "$service_package/claude-code/"
cat >"$service_package/appa-runtime-v2" <<EOF
#!/usr/bin/env python3
"""A stub runtime: it reports its version and serves the health endpoint."""
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

arguments = sys.argv[1:]
if "--version" in arguments:
    print("appa-runtime-v2 $version")
    raise SystemExit(0)

instance_id = ""
for position, argument in enumerate(arguments):
    if argument == "--instance-id" and position + 1 < len(arguments):
        instance_id = arguments[position + 1]

class Health(BaseHTTPRequestHandler):
    def do_GET(self):
        body = b"ok" if self.path == "/health" else b"{}"
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("X-Appa-Instance-Id", instance_id)
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *arguments):
        pass

HTTPServer(("127.0.0.1", 8787), Health).serve_forever()
EOF
chmod 755 "$service_package/appa-runtime-v2"
tar -C "$service_package" -czf "$service_release/$archive" .
printf '%s\n' "$version" >"$service_release/version.txt"
if command -v sha256sum >/dev/null 2>&1; then
  service_checksum=$(sha256sum "$service_release/$archive")
else
  service_checksum=$(shasum -a 256 "$service_release/$archive")
fi
service_checksum=${service_checksum%% *}
printf '%s  %s\n' "$service_checksum" "$archive" >"$service_release/SHA256SUMS"

export APPA_DOWNLOAD_BASE="file://$service_release"
export APPA_INSTALL_DIR="$tmp_dir/service/bin"
export APPA_CONFIG_DIR="$tmp_dir/service/config"
export APPA_DATA_DIR="$tmp_dir/service/data"
export APPA_SKIP_SERVICE=0
linger_before=no
if loginctl show-user "$(id -un)" 2>/dev/null | grep -q '^Linger=yes$'; then
  linger_before=yes
fi
remove_service() {
  APPA_SKIP_SERVICE=0 sh "$repository_root/install.sh" --uninstall >/dev/null 2>&1 || true
  if [ "$linger_before" = no ]; then
    loginctl disable-linger "$(id -un)" >/dev/null 2>&1 || true
  fi
  cleanup
}
trap remove_service 0 1 2 15

service_output=$(sh "$repository_root/install.sh")
printf '%s\n' "$service_output" | grep -F "running in the background" >/dev/null ||
  die "installer did not report background startup"

curl --silent --max-time 2 http://127.0.0.1:8787/health | grep -q '^ok$' ||
  die "runtime is not serving http://127.0.0.1:8787/health"
service_pid=$(pgrep -f "$APPA_INSTALL_DIR/appa-runtime-v2" | head -n 1)
[ -n "$service_pid" ] || die "no runtime process runs from $APPA_INSTALL_DIR"
service_tty=$(ps -o tty= -p "$service_pid" | tr -d ' ')
case "$service_tty" in
  '?'|'??'|'') ;;
  *) die "runtime holds the controlling terminal $service_tty" ;;
esac
service_parent=$(ps -o ppid= -p "$service_pid" | tr -d ' ')
service_parent_command=$(ps -o comm= -p "$service_parent" | tr -d ' ')

case "$target_os" in
  unknown-linux-gnu)
    [ -f "$unit" ] || die "systemd user unit was not installed"
    systemctl --user is-enabled --quiet "$service_name" ||
      die "systemd user service is not enabled for startup"
    systemctl --user is-active --quiet "$service_name" ||
      die "systemd user service is not running"
    if grep -q network-online "$unit"; then
      die "user unit orders itself after a target the user manager does not have"
    fi
    case "$service_parent_command" in
      *systemd*) ;;
      *) die "runtime runs under $service_parent_command, not the systemd user manager" ;;
    esac
    if printf '%s\n' "$service_output" | grep -F "starts at boot" >/dev/null; then
      loginctl show-user "$(id -un)" 2>/dev/null | grep -q '^Linger=yes$' ||
        die "installer reported boot startup without lingering enabled"
    else
      printf '%s\n' "$service_output" | grep -F "loginctl enable-linger" >/dev/null ||
        die "installer neither enabled lingering nor named the command that does"
    fi
    ;;
  apple-darwin)
    [ -f "$plist" ] || die "LaunchAgent was not installed"
    grep -F RunAtLoad "$plist" >/dev/null || die "LaunchAgent does not start at login"
    launchctl print "gui/$(id -u)/$launchd_label" 2>/dev/null |
      grep -q 'state = running' || die "LaunchAgent is not running"
    case "$service_parent_command" in
      launchd|*/launchd) ;;
      *) die "runtime runs under $service_parent_command, not launchd" ;;
    esac
    ;;
esac

sh "$repository_root/install.sh" --uninstall >/dev/null
[ ! -e "$unit" ] || die "systemd user unit survived uninstall"
[ ! -e "$plist" ] || die "LaunchAgent survived uninstall"
if curl --silent --max-time 2 http://127.0.0.1:8787/health >/dev/null 2>&1; then
  die "runtime kept serving after uninstall"
fi
if [ "$linger_before" = no ]; then
  loginctl disable-linger "$(id -un)" >/dev/null 2>&1 || true
fi
trap cleanup 0 1 2 15

printf 'Startup registration test passed on %s.\n' "$target_os"
printf 'Unix installer tests passed.\n'
