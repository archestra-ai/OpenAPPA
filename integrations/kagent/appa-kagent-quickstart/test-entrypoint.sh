#!/bin/sh
set -eu

entrypoint=$(cd "$(dirname "$0")" && pwd)/entrypoint.sh
work=$(mktemp -d)
runtime_pid=""
cleanup() {
  if [ -z "$runtime_pid" ] && [ -f "$work/runtime-pid" ]; then
    runtime_pid=$(cat "$work/runtime-pid")
  fi
  if [ -n "$runtime_pid" ]; then
    kill "$runtime_pid" 2>/dev/null || true
  fi
  rm -rf "$work"
}
trap cleanup EXIT
mkdir -p "$work/bin" "$work/data"

cat >"$work/bin/appa" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >"$TEST_WORK/appa-args"
printf '%s\n' "$$" >"$TEST_WORK/runtime-pid"
while :; do sleep 1; done
EOF
cat >"$work/bin/curl" <<'EOF'
#!/bin/sh
exit 0
EOF
cat >"$work/bin/appa-kagent-adk" <<'EOF'
#!/bin/sh
printf '%s\n' "$APPA_CONFIG" >"$TEST_WORK/config-path"
cp "$APPA_CONFIG" "$TEST_WORK/config-copy"
EOF
chmod 0755 "$work/bin/appa" "$work/bin/curl" "$work/bin/appa-kagent-adk"

policy='[policy]
version = 2
'
PATH="$work/bin:$PATH" \
  TEST_WORK="$work" \
  APPA_ENABLED=true \
  APPA_CONFIG=/opt/appa/kagent.appa.toml \
  APPA_CONFIG_CONTENTS="$policy" \
  APPA_DATA_DIR="$work/data" \
  APPA_DB="$work/data/appa.db" \
  "$entrypoint"

runtime_pid=$(cat "$work/runtime-pid")
test "$(cat "$work/config-path")" = "$work/data/appa.toml"
cmp "$work/config-copy" "$work/data/appa.toml"
grep -F -- "--config $work/data/appa.toml" "$work/appa-args" >/dev/null
grep -F -- "--db $work/data/appa.db" "$work/appa-args" >/dev/null

echo "test-entrypoint: bundled policy contents passed"
