#!/usr/bin/env bash
# Build each release independently: their SQLite libraries cannot share a link.
set -euo pipefail
: "${OPENAPPA_TEST_DATABASE_URL:?Set OPENAPPA_TEST_DATABASE_URL to the test PostgreSQL store}"
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
build_dir="$script_dir/target"
cargo build --locked --manifest-path "$script_dir/previous/Cargo.toml" --target-dir "$build_dir"
cargo build --locked --manifest-path "$script_dir/Cargo.toml" --target-dir "$build_dir"
exec "$build_dir/debug/appa-mixed-version"
