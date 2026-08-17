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

output=$(sh "$repository_root/install.sh")
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

printf 'policy survives update\n' >"$APPA_CONFIG_DIR/appa.toml"
printf 'database survives update\n' >"$APPA_DATA_DIR/appa.db"
sh "$repository_root/install.sh" >/dev/null
[ "$(cat "$APPA_CONFIG_DIR/appa.toml")" = "policy survives update" ] || die "policy was replaced"
[ "$(cat "$APPA_DATA_DIR/appa.db")" = "database survives update" ] || die "database was replaced"

printf 'tampered\n' >>"$release_dir/$archive"
if sh "$repository_root/install.sh" >"$tmp_dir/tampered.stdout" 2>"$tmp_dir/tampered.stderr"; then
  die "tampered archive was accepted"
fi
grep -F "checksum mismatch for $archive" "$tmp_dir/tampered.stderr" >/dev/null ||
  die "tampered archive did not fail at checksum verification"
[ "$($installed_binary --version)" = "appa-runtime-v2 $version" ] ||
  die "failed update changed installed runtime"

sh "$repository_root/install.sh" --uninstall >/dev/null
[ ! -e "$installed_binary" ] || die "runtime survived uninstall"
[ ! -e "$installed_plugin" ] || die "plugin survived uninstall"
[ "$(cat "$APPA_CONFIG_DIR/appa.toml")" = "policy survives update" ] ||
  die "uninstall removed policy"
[ "$(cat "$APPA_DATA_DIR/appa.db")" = "database survives update" ] ||
  die "uninstall removed database"

printf 'Unix installer tests passed.\n'
