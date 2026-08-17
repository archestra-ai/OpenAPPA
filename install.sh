#!/bin/sh
set -eu

die() {
  printf 'appa installer: %s\n' "$*" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Usage: sh install.sh [--upgrade | --uninstall]

With no option the installer refuses to touch an existing installation. It
reports the installed version beside the release version and exits. Pass
--upgrade to replace the installed runtime with the release.

Environment overrides:
  APPA_VERSION         Release version to install, without or with a leading v
  APPA_INSTALL_DIR     Binary installation directory
  APPA_CONFIG_DIR      Policy configuration directory
  APPA_DATA_DIR        Database and Claude plugin directory
  APPA_REPOSITORY      GitHub repository (default: archestra-ai/OpenAPPA)
  APPA_DOWNLOAD_BASE   Complete release-asset base URL
  APPA_SKIP_SERVICE    Set to 1 to skip login startup and health verification
EOF
}

uninstall=0
upgrade=0
case "${1:-}" in
  '') ;;
  --uninstall) uninstall=1 ;;
  --upgrade) upgrade=1 ;;
  -h|--help) usage; exit 0 ;;
  *) usage >&2; exit 2 ;;
esac

: "${HOME:?HOME must be set}"

repository=${APPA_REPOSITORY:-archestra-ai/OpenAPPA}
install_dir=${APPA_INSTALL_DIR:-"$HOME/.local/bin"}
skip_service=${APPA_SKIP_SERVICE:-0}
service_name=appa-runtime-v2.service
launchd_label=ai.archestra.appa-runtime-v2
[ "$skip_service" = 0 ] || [ "$skip_service" = 1 ] ||
  die "APPA_SKIP_SERVICE must be 0 or 1"

case "$(uname -s)" in
  Linux)
    platform=linux
    config_dir=${APPA_CONFIG_DIR:-"${XDG_CONFIG_HOME:-$HOME/.config}/appa"}
    data_dir=${APPA_DATA_DIR:-"${XDG_DATA_HOME:-$HOME/.local/share}/appa"}
    ;;
  Darwin)
    platform=darwin
    config_dir=${APPA_CONFIG_DIR:-"$HOME/Library/Application Support/appa"}
    data_dir=${APPA_DATA_DIR:-"$HOME/Library/Application Support/appa"}
    ;;
  *) die "supported systems are Linux and macOS" ;;
esac

for directory in "$install_dir" "$config_dir" "$data_dir"; do
  case "$directory" in
    /*) ;;
    *) die "installation directories must be absolute: $directory" ;;
  esac
done

binary=$install_dir/appa-runtime-v2
config_file=$config_dir/appa.toml
db_file=$data_dir/appa.db
plugin_dir=$data_dir/claude-code

remove_service() {
  case "$platform" in
    linux)
      unit_dir=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user
      unit=$unit_dir/$service_name
      if [ -f "$unit" ]; then
        command -v systemctl >/dev/null 2>&1 ||
          die "systemctl is required to remove the installed user service"
        systemctl --user disable --now "$service_name" >/dev/null ||
          die "could not stop the installed systemd user service"
        rm -f "$unit"
        systemctl --user daemon-reload >/dev/null ||
          die "could not reload the systemd user manager"
      fi
      ;;
    darwin)
      plist=$HOME/Library/LaunchAgents/$launchd_label.plist
      if [ -f "$plist" ]; then
        command -v launchctl >/dev/null 2>&1 ||
          die "launchctl is required to remove the installed LaunchAgent"
        domain=gui/$(id -u)
        if launchctl print "$domain/$launchd_label" >/dev/null 2>&1; then
          launchctl bootout "$domain/$launchd_label" >/dev/null ||
            die "could not stop the installed LaunchAgent"
        fi
        rm -f "$plist"
      fi
      ;;
  esac
}

if [ "$uninstall" = 1 ]; then
  remove_service
  rm -f "$binary"
  rm -rf "$plugin_dir"
  [ ! -e "$binary" ] || die "could not remove $binary"
  [ ! -e "$plugin_dir" ] || die "could not remove $plugin_dir"
  printf 'Removed appa-runtime-v2 and Claude Code integration files.\n'
  printf 'Preserved policy: %s\n' "$config_file"
  printf 'Preserved database: %s\n' "$db_file"
  exit 0
fi

command -v curl >/dev/null 2>&1 || die "curl is required"
command -v grep >/dev/null 2>&1 || die "grep is required"
command -v tar >/dev/null 2>&1 || die "tar is required"

case "$(uname -m)" in
  x86_64|amd64) architecture=x86_64 ;;
  arm64|aarch64) architecture=aarch64 ;;
  *) die "unsupported architecture: $(uname -m)" ;;
esac

case "$platform" in
  linux) target=$architecture-unknown-linux-gnu ;;
  darwin) target=$architecture-apple-darwin ;;
esac
archive=appa-runtime-v2-$target.tar.gz

requested_version=${APPA_VERSION:-}
requested_version=${requested_version#v}
if [ -n "$requested_version" ] &&
  ! printf '%s\n' "$requested_version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.-]+)?$'; then
  die "invalid APPA_VERSION: $requested_version"
fi

tmp_dir=$(mktemp -d 2>/dev/null || mktemp -d -t appa-install)
instance_id=appa-$(basename "$tmp_dir")-$$
cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup 0 1 2 15

release_tag=
if [ -n "$requested_version" ]; then
  release_tag=v$requested_version
fi

use_gh=0
if [ -z "${APPA_DOWNLOAD_BASE:-}" ] && command -v gh >/dev/null 2>&1 &&
  gh auth status --hostname github.com >/dev/null 2>&1; then
  use_gh=1
fi

if [ -n "${APPA_DOWNLOAD_BASE:-}" ]; then
  asset_base=${APPA_DOWNLOAD_BASE%/}
elif [ -n "$release_tag" ]; then
  asset_base=https://github.com/$repository/releases/download/$release_tag
else
  asset_base=https://github.com/$repository/releases/latest/download
fi
case "$asset_base" in
  https://*|file://*) ;;
  http://*)
    http_authority=${asset_base#http://}
    http_authority=${http_authority%%/*}
    printf '%s\n' "$http_authority" |
      grep -Eq '^(127\.0\.0\.1|localhost)(:[0-9]+)?$' ||
      die "APPA_DOWNLOAD_BASE permits HTTP only for an exact loopback host"
    ;;
  *) die "APPA_DOWNLOAD_BASE must use HTTPS, file, or loopback HTTP" ;;
esac

download_asset() {
  asset_name=$1
  destination=$2
  rm -f "$destination"

  if [ "$use_gh" = 1 ]; then
    if [ -n "$release_tag" ]; then
      gh release download "$release_tag" --repo "$repository" --pattern "$asset_name" \
        --dir "$tmp_dir" --clobber >/dev/null
    else
      gh release download --repo "$repository" --pattern "$asset_name" \
        --dir "$tmp_dir" --clobber >/dev/null
    fi
    [ -f "$destination" ] || die "release asset not found: $asset_name"
    return
  fi

  if [ -n "${APPA_DOWNLOAD_BASE:-}" ]; then
    curl_options='-fsSL'
  else
    curl_options="--proto =https --tlsv1.2 -fsSL"
  fi
  # shellcheck disable=SC2086
  curl $curl_options "$asset_base/$asset_name" -o "$destination" ||
    die "could not download $asset_name; authenticate gh for a private repository"
}

download_asset version.txt "$tmp_dir/version.txt"
version=$(tr -d '\r\n' <"$tmp_dir/version.txt")
if ! printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.-]+)?$'; then
  die "release contains an invalid version.txt"
fi
if [ -n "$requested_version" ] && [ "$version" != "$requested_version" ]; then
  die "requested $requested_version but release reports $version"
fi

# Prints older, same or newer for how the first text orders against the second.
compare_text() {
  if [ "$1" = "$2" ]; then
    printf 'same\n'
  elif [ "$(printf '%s\n%s\n' "$1" "$2" | LC_ALL=C sort | head -n 1)" = "$1" ]; then
    printf 'older\n'
  else
    printf 'newer\n'
  fi
}

# Prints older, same or newer for one pair of version fields. Two numeric
# fields compare numerically. Any other pair compares as text.
compare_fields() {
  case "$1$2" in
    *[!0-9]*)
      compare_text "$1" "$2"
      return
      ;;
  esac
  if [ "$1" -lt "$2" ]; then
    printf 'older\n'
  elif [ "$1" -gt "$2" ]; then
    printf 'newer\n'
  else
    printf 'same\n'
  fi
}

# Prints older, same or newer for how the first version orders against the
# second. The text before the first hyphen decides first: it compares field by
# dotted field, and a field absent from one side counts as 0. On equal fields a
# version carrying a prerelease suffix orders before one without it, and two
# suffixes compare as text.
compare_versions() {
  left_core=${1%%-*}
  left_suffix=${1#"$left_core"}
  right_core=${2%%-*}
  right_suffix=${2#"$right_core"}

  while [ -n "$left_core" ] || [ -n "$right_core" ]; do
    left_field=${left_core%%.*}
    right_field=${right_core%%.*}
    case "$left_core" in
      *.*) left_core=${left_core#*.} ;;
      *) left_core= ;;
    esac
    case "$right_core" in
      *.*) right_core=${right_core#*.} ;;
      *) right_core= ;;
    esac
    field_order=$(compare_fields "${left_field:-0}" "${right_field:-0}")
    if [ "$field_order" != same ]; then
      printf '%s\n' "$field_order"
      return
    fi
  done

  if [ "$left_suffix" = "$right_suffix" ]; then
    printf 'same\n'
  elif [ -z "$left_suffix" ]; then
    printf 'newer\n'
  elif [ -z "$right_suffix" ]; then
    printf 'older\n'
  else
    compare_text "$left_suffix" "$right_suffix"
  fi
}

installed_version=
if [ -f "$binary" ] && installed_report=$("$binary" --version 2>/dev/null); then
  case "$installed_report" in
    'appa-runtime-v2 '*) installed_version=${installed_report#appa-runtime-v2 } ;;
  esac
fi

upgrade_command="curl -fsSL https://github.com/$repository/releases/latest/download/install.sh | sh -s -- --upgrade"

if [ "$upgrade" = 0 ] && [ -e "$binary" ]; then
  if [ -z "$installed_version" ]; then
    printf 'appa-runtime-v2 is installed, but it does not report a version.\n'
  else
    case "$(compare_versions "$installed_version" "$version")" in
      same) printf 'appa-runtime-v2 %s is already installed.\n' "$installed_version" ;;
      newer) printf 'The installed appa-runtime-v2 is newer than this release.\n' ;;
      older) printf 'A newer appa-runtime-v2 release is available.\n' ;;
    esac
  fi
  printf 'Installed: %s\n' "${installed_version:-unknown}"
  printf 'Release: %s\n' "$version"
  printf 'Runtime: %s\n' "$binary"
  printf 'Nothing changed. Install the release over it with:\n  %s\n' "$upgrade_command"
  exit 0
fi

download_asset SHA256SUMS "$tmp_dir/SHA256SUMS"
download_asset "$archive" "$tmp_dir/$archive"

expected_checksum=
checksum_matches=0
while IFS= read -r checksum_line; do
  checksum=${checksum_line%% *}
  checksum_file=${checksum_line#* }
  checksum_file=${checksum_file# }
  checksum_file=${checksum_file#\*}
  if [ "$checksum_file" = "$archive" ]; then
    if [ "${#checksum}" -ne 64 ] || printf '%s\n' "$checksum" | grep -Eq '[^0-9A-Fa-f]'; then
      die "invalid checksum for $archive"
    fi
    expected_checksum=$checksum
    checksum_matches=$((checksum_matches + 1))
  fi
done <"$tmp_dir/SHA256SUMS"
[ "$checksum_matches" -eq 1 ] || die "SHA256SUMS must name $archive exactly once"

if command -v sha256sum >/dev/null 2>&1; then
  actual_checksum=$(sha256sum "$tmp_dir/$archive")
elif command -v shasum >/dev/null 2>&1; then
  actual_checksum=$(shasum -a 256 "$tmp_dir/$archive")
else
  die "sha256sum or shasum is required"
fi
actual_checksum=${actual_checksum%% *}
expected_checksum=$(printf '%s' "$expected_checksum" | tr 'A-F' 'a-f')
actual_checksum=$(printf '%s' "$actual_checksum" | tr 'A-F' 'a-f')
[ "$actual_checksum" = "$expected_checksum" ] || die "checksum mismatch for $archive"

extract_dir=$tmp_dir/extract
mkdir -p "$extract_dir"
tar -xzf "$tmp_dir/$archive" -C "$extract_dir"
source_binary=$extract_dir/appa-runtime-v2
[ -x "$source_binary" ] || die "$archive does not contain appa-runtime-v2"
[ -f "$extract_dir/claude-code/.claude-plugin/marketplace.json" ] ||
  die "$archive does not contain the Claude Code marketplace"
[ -f "$extract_dir/claude-code/plugin/.claude-plugin/plugin.json" ] ||
  die "$archive does not contain the Claude Code plugin manifest"
[ -f "$extract_dir/claude-code/plugin/.mcp.json" ] ||
  die "$archive does not contain the Claude Code MCP configuration"
[ -f "$extract_dir/claude-code/plugin/hooks/hooks.json" ] ||
  die "$archive does not contain the Claude Code plugin"
[ -f "$extract_dir/claude-code/plugin/hooks/session-context.md" ] ||
  die "$archive does not contain the Claude Code session context"
[ -f "$extract_dir/claude-code/plugin/skills/appa-tool-sync/SKILL.md" ] ||
  die "$archive does not contain the Claude Code tool-sync skill"
[ -f "$extract_dir/claude-code/plugin/statusline.sh" ] ||
  die "$archive does not contain the Claude Code statusline"

if ! reported_version=$($source_binary --version); then
  if [ "$platform" = linux ]; then
    die "binary cannot run on this system; Linux releases require glibc"
  fi
  die "binary cannot run on this macOS system"
fi
[ "$reported_version" = "appa-runtime-v2 $version" ] ||
  die "binary reports '$reported_version', expected 'appa-runtime-v2 $version'"
supports_instance_id=0
if "$source_binary" --instance-id installer-probe --version >/dev/null 2>&1; then
  supports_instance_id=1
fi

service_backup=$tmp_dir/service.backup
had_service=0
service_was_active=0
service_was_enabled=0
service_was_loaded=0
if [ "$skip_service" = 0 ]; then
  case "$platform" in
    linux)
      service_file=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/$service_name
      if [ -f "$service_file" ]; then
        cp "$service_file" "$service_backup"
        had_service=1
        if command -v systemctl >/dev/null 2>&1 &&
          systemctl --user is-enabled --quiet "$service_name"; then
          service_was_enabled=1
        fi
        if command -v systemctl >/dev/null 2>&1 &&
          systemctl --user is-active --quiet "$service_name"; then
          service_was_active=1
        fi
      fi
      ;;
    darwin)
      service_file=$HOME/Library/LaunchAgents/$launchd_label.plist
      if [ -f "$service_file" ]; then
        cp "$service_file" "$service_backup"
        had_service=1
        if command -v launchctl >/dev/null 2>&1; then
          if launchctl print "gui/$(id -u)/$launchd_label" >/dev/null 2>&1; then
            service_was_loaded=1
          fi
          if launchctl print "gui/$(id -u)/$launchd_label" 2>/dev/null |
            grep -q 'state = running'; then
            service_was_active=1
          fi
        fi
      fi
      ;;
  esac
fi

umask 077
mkdir -p "$install_dir" "$config_dir" "$data_dir"
chmod 700 "$config_dir" "$data_dir"

plugin_new=$plugin_dir.new.$$
plugin_old=$plugin_dir.old.$$
had_plugin=0
rm -rf "$plugin_new" "$plugin_old"
mkdir -p "$plugin_new"
cp -R "$extract_dir/claude-code/." "$plugin_new/"
chmod 755 "$plugin_new/plugin/statusline.sh"

binary_new=$binary.new.$$
binary_old=$binary.old.$$
had_binary=0
rm -f "$binary_new" "$binary_old"
cp "$source_binary" "$binary_new"
chmod 755 "$binary_new"
if [ -f "$binary" ]; then
  if ! mv "$binary" "$binary_old"; then
    rm -rf "$plugin_new"
    die "could not prepare the installed runtime for update"
  fi
  had_binary=1
fi
if ! mv "$binary_new" "$binary"; then
  [ "$had_binary" = 0 ] || mv "$binary_old" "$binary"
  rm -rf "$plugin_new"
  die "could not install appa-runtime-v2"
fi

if [ -d "$plugin_dir" ]; then
  if ! mv "$plugin_dir" "$plugin_old"; then
    rm -f "$binary"
    [ "$had_binary" = 0 ] || mv "$binary_old" "$binary"
    rm -rf "$plugin_new"
    die "could not prepare the Claude Code plugin for update"
  fi
  had_plugin=1
fi
if ! mv "$plugin_new" "$plugin_dir"; then
  [ ! -d "$plugin_old" ] || mv "$plugin_old" "$plugin_dir"
  rm -f "$binary"
  [ "$had_binary" = 0 ] || mv "$binary_old" "$binary"
  die "could not install Claude Code plugin files"
fi

systemd_escape() {
  printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g; s/%/%%/g; s/\$/$$/g'
}

xml_escape() {
  printf '%s' "$1" | sed 's/&/\&amp;/g; s/</\&lt;/g; s/>/\&gt;/g'
}

configure_service() {
  if [ "$skip_service" = 1 ]; then
    return
  fi

  case "$platform" in
    linux)
      command -v systemctl >/dev/null 2>&1 ||
        die "systemd user services are required; set APPA_SKIP_SERVICE=1 for manual startup"
      systemctl --user show-environment >/dev/null 2>&1 ||
        die "systemd user manager is unavailable; set APPA_SKIP_SERVICE=1 for manual startup"
      unit_dir=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user
      unit=$unit_dir/$service_name
      mkdir -p "$unit_dir"
      escaped_binary=$(systemd_escape "$binary")
      escaped_config=$(systemd_escape "$config_file")
      escaped_db=$(systemd_escape "$db_file")
      instance_argument=
      if [ "$supports_instance_id" = 1 ]; then
        instance_argument=" --instance-id \"$instance_id\""
      fi
      cat >"$unit" <<EOF
[Unit]
Description=OpenAPPA flow runtime
After=network-online.target

[Service]
Type=simple
ExecStart="$escaped_binary" --config "$escaped_config" --db "$escaped_db"$instance_argument
UMask=0077
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
EOF
      systemctl --user daemon-reload
      systemctl --user enable "$service_name" >/dev/null
      systemctl --user stop "$service_name" >/dev/null 2>&1 || true
      if [ "$supports_instance_id" = 0 ] &&
        curl --silent --max-time 1 http://127.0.0.1:8787/health >/dev/null 2>&1; then
        die "port 8787 is already in use by a process outside the installed service"
      fi
      systemctl --user start "$service_name"
      ;;
    darwin)
      command -v launchctl >/dev/null 2>&1 || die "launchctl is required"
      launch_agents=$HOME/Library/LaunchAgents
      plist=$launch_agents/$launchd_label.plist
      mkdir -p "$launch_agents"
      escaped_binary=$(xml_escape "$binary")
      escaped_config=$(xml_escape "$config_file")
      escaped_db=$(xml_escape "$db_file")
      escaped_stdout=$(xml_escape "$data_dir/runtime.stdout.log")
      escaped_stderr=$(xml_escape "$data_dir/runtime.stderr.log")
      instance_arguments=
      if [ "$supports_instance_id" = 1 ]; then
        instance_arguments="    <string>--instance-id</string>
    <string>$instance_id</string>"
      fi
      cat >"$plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>$launchd_label</string>
  <key>ProgramArguments</key>
  <array>
    <string>$escaped_binary</string>
    <string>--config</string>
    <string>$escaped_config</string>
    <string>--db</string>
    <string>$escaped_db</string>
$instance_arguments
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>Umask</key>
  <integer>63</integer>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
  <key>StandardOutPath</key>
  <string>$escaped_stdout</string>
  <key>StandardErrorPath</key>
  <string>$escaped_stderr</string>
</dict>
</plist>
EOF
      domain=gui/$(id -u)
      launchctl bootout "$domain/$launchd_label" >/dev/null 2>&1 || true
      if [ "$supports_instance_id" = 0 ] &&
        curl --silent --max-time 1 http://127.0.0.1:8787/health >/dev/null 2>&1; then
        die "port 8787 is already in use by a process outside the installed LaunchAgent"
      fi
      launchctl bootstrap "$domain" "$plist"
      launchctl kickstart -k "$domain/$launchd_label"
      ;;
  esac
}

wait_for_runtime() {
  if [ "$skip_service" = 1 ]; then
    return
  fi

  attempts=0
  while [ "$attempts" -lt 30 ]; do
    health_body=$tmp_dir/health.body
    health_headers=$tmp_dir/health.headers
    if curl --fail --silent --max-time 1 -D "$health_headers" \
      -o "$health_body" http://127.0.0.1:8787/health 2>/dev/null; then
      health=$(cat "$health_body")
      instance_header=$(grep -i '^x-appa-instance-id:' "$health_headers" 2>/dev/null || true)
      returned_instance_id=${instance_header#*:}
      returned_instance_id=$(printf '%s' "$returned_instance_id" | tr -d ' \r\n')
    else
      health=
      returned_instance_id=
    fi
    health_is_ours=false
    if [ "$supports_instance_id" = 0 ] || [ "$returned_instance_id" = "$instance_id" ]; then
      health_is_ours=true
    fi
    if [ "$health" = ok ] && [ "$health_is_ours" = true ]; then
      case "$platform" in
        linux)
          systemctl --user is-active --quiet "$service_name" ||
            die "runtime endpoint is healthy, but the installed systemd service is not active"
          ;;
        darwin)
          launchctl print "gui/$(id -u)/$launchd_label" 2>/dev/null |
            grep -q 'state = running' ||
            die "runtime endpoint is healthy, but the installed LaunchAgent is not running"
          ;;
      esac
      return
    fi
    attempts=$((attempts + 1))
    sleep 1
  done
  if [ "$platform" = linux ]; then
    systemctl --user status "$service_name" --no-pager >&2 || true
  fi
  die "runtime did not become healthy at http://127.0.0.1:8787/health"
}

rollback_install() {
  if [ "$skip_service" = 0 ]; then
    case "$platform" in
      linux)
        systemctl --user stop "$service_name" >/dev/null 2>&1 || true
        systemctl --user disable "$service_name" >/dev/null 2>&1 || true
        if [ "$had_service" = 1 ]; then
          cp "$service_backup" "$service_file"
        else
          rm -f "$service_file"
        fi
        systemctl --user daemon-reload >/dev/null 2>&1 || true
        if [ "$had_service" = 1 ] && [ "$service_was_enabled" = 1 ]; then
          systemctl --user enable "$service_name" >/dev/null 2>&1 || true
        fi
        ;;
      darwin)
        domain=gui/$(id -u)
        launchctl bootout "$domain/$launchd_label" >/dev/null 2>&1 || true
        if [ "$had_service" = 1 ]; then
          cp "$service_backup" "$service_file"
        else
          rm -f "$service_file"
        fi
        ;;
    esac
  fi

  rm -f "$binary"
  [ "$had_binary" = 0 ] || mv "$binary_old" "$binary"
  rm -rf "$plugin_dir"
  [ "$had_plugin" = 0 ] || mv "$plugin_old" "$plugin_dir"

  if [ "$skip_service" = 0 ] && [ "$had_service" = 1 ]; then
    case "$platform" in
      linux)
        if [ "$service_was_active" = 1 ]; then
          systemctl --user start "$service_name" >/dev/null 2>&1 || true
        fi
        ;;
      darwin)
        if [ "$service_was_loaded" = 1 ]; then
          launchctl bootstrap "gui/$(id -u)" "$service_file" >/dev/null 2>&1 || true
        fi
        ;;
    esac
  fi
}

if ! (configure_service && wait_for_runtime); then
  rollback_install
  exit 1
fi
rm -f "$binary_old"
rm -rf "$plugin_old"

if [ -n "$installed_version" ] && [ "$installed_version" != "$version" ]; then
  printf 'Replaced appa-runtime-v2 %s.\n' "$installed_version"
fi
printf 'Installed appa-runtime-v2 %s.\n' "$version"
printf 'Runtime: %s\n' "$binary"
printf 'Policy: %s\n' "$config_file"
printf 'Database: %s\n' "$db_file"
printf 'Claude plugin: %s\n' "$plugin_dir/plugin"
if [ "$skip_service" = 1 ]; then
  printf 'Login startup skipped. Start manually:\n  "%s" --config "%s" --db "%s"\n' \
    "$binary" "$config_file" "$db_file"
fi
if command -v claude >/dev/null 2>&1; then
  printf 'Claude Code detected.\n'
else
  printf 'Claude Code not found. Install it before configuring a gated session.\n'
fi
printf 'Ask an ungated Claude Code session to configure OpenAPPA with:\n'
printf '  settings overlay: %s/.claude/appa-session-settings.json\n' "$HOME"
printf '  statusline: %s/statusline.sh\n' "$plugin_dir/plugin"
printf '  plugin: %s\n' "$plugin_dir/plugin"
printf 'Add a clappa alias that passes both --settings and --plugin-dir.\n'
printf 'Only clappa sessions are gated. They block while the runtime service is down.\n'
printf 'Start clappa and run /appa-tool-sync to declare installed MCP tools.\n'
