#!/usr/bin/env bash
# Regenerate the official catalog from tracked and non-ignored working files.
set -euo pipefail

case "${1:-}" in
  ""|--check) ;;
  *) echo "usage: bash scripts/appa-marketplace.sh [--check]" >&2; exit 2 ;;
esac
root="$(git rev-parse --show-toplevel)"
cd "$root"
stage="$(mktemp -d "${TMPDIR:-/tmp}/appa-marketplace.XXXXXX")"
trap 'rm -rf -- "$stage"' EXIT

# Exclude ignored build/test artifacts, include new packages, and read current
# file contents so regeneration works before staging or committing changes.
git ls-files -z --cached --others --exclude-standard -- marketplace |
  while IFS= read -r -d '' path; do
    if [[ -e "$path" || -L "$path" ]]; then printf '%s\0' "$path"; fi
  done > "$stage/files"
tar --null -T "$stage/files" -cf "$stage/source.tar"
tar -xf "$stage/source.tar" -C "$stage"
cargo run --quiet --locked -p appa-package --example marketplace -- "$stage/marketplace" > "$stage/catalog.toml"
if [[ "${1:-}" == --check ]]; then
  if ! diff -u marketplace/marketplace.toml "$stage/catalog.toml"; then
    echo "Regenerate the catalog: bash scripts/appa-marketplace.sh" >&2
    exit 1
  fi
else
  cp "$stage/catalog.toml" marketplace/marketplace.toml
fi
