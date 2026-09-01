#!/bin/sh
# Install the released `appa` binary for this machine:
#
#   curl -fsSL https://openappa.com/install.sh | sh
#
# Every release ships one archive per platform beside a SHA256SUMS list. The
# script resolves the latest release tag (or `APPA_VERSION`), downloads the
# archive and the list from that one release, verifies the digest, and installs
# `appa` into `APPA_INSTALL_DIR` (default `~/.local/bin`). It never runs
# `appa init`: init can prompt, and under a pipe stdin is the script itself.
#
# Linux and macOS on x86_64 and aarch64 only. Windows users unpack the zip from
# the releases page. `APPA_REPOSITORY_URL` exists for appa-install-test.sh,
# which serves a local release.
set -eu

repository=${APPA_REPOSITORY_URL:-https://github.com/archestra-ai/OpenAPPA}
tag_pattern='^v[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.-]+)?$'

fail() {
  printf 'appa-install: %s\n' "$1" >&2
  exit 1
}

command -v curl >/dev/null 2>&1 || fail "curl is required"

if command -v sha256sum >/dev/null 2>&1; then
  digest() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then
  digest() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
  fail "sha256sum or shasum is required to verify the download"
fi

system=$(uname -s)
machine=$(uname -m)
case $system in
  Linux) platform=unknown-linux-gnu ;;
  Darwin) platform=apple-darwin ;;
  *) fail "no release binary for $system; see $repository/releases" ;;
esac
case $machine in
  x86_64 | amd64) architecture=x86_64 ;;
  aarch64 | arm64) architecture=aarch64 ;;
  *) fail "no release binary for $system $machine; see $repository/releases" ;;
esac
archive=appa-$architecture-$platform.tar.gz

if [ -n "${APPA_INSTALL_DIR:-}" ]; then
  install_dir=$APPA_INSTALL_DIR
elif [ -n "${HOME:-}" ]; then
  install_dir=$HOME/.local/bin
else
  fail "set APPA_INSTALL_DIR: HOME is not set"
fi
# Hooks and `clappa` are rendered with this path, from any working directory.
case $install_dir in
  /*) ;;
  *) fail "APPA_INSTALL_DIR must be an absolute path" ;;
esac

if [ -n "${APPA_VERSION:-}" ]; then
  tag=$APPA_VERSION
else
  latest=$(curl -fsS -o /dev/null -w '%{redirect_url}' "$repository/releases/latest") ||
    fail "could not resolve the latest release from $repository"
  case $latest in
    "$repository/releases/tag/"*) tag=${latest#"$repository/releases/tag/"} ;;
    *) fail "unexpected redirect for the latest release: $latest" ;;
  esac
fi
printf '%s\n' "$tag" | grep -Eq "$tag_pattern" || fail "not a release tag: $tag"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# The list and the archive come from the same pinned release, never from
# `latest`, so a release published mid-run cannot pair one with the other.
release=$repository/releases/download/$tag
printf 'Downloading appa %s for %s-%s\n' "$tag" "$architecture" "$platform" >&2
curl -fsSL -o "$work/SHA256SUMS" "$release/SHA256SUMS" ||
  fail "could not download $release/SHA256SUMS"
curl -fsSL -o "$work/$archive" "$release/$archive" ||
  fail "could not download $release/$archive"

listed=$(grep -E "^[0-9a-f]{64}  $archive\$" "$work/SHA256SUMS" || true)
[ "$(printf '%s\n' "$listed" | grep -c .)" -eq 1 ] ||
  fail "SHA256SUMS does not list $archive exactly once"
expected=${listed%% *}
actual=$(digest "$work/$archive")
[ "$actual" = "$expected" ] || fail "checksum mismatch for $archive"

mkdir "$work/extract"
tar -xzf "$work/$archive" -C "$work/extract"
[ -f "$work/extract/appa" ] || fail "$archive does not contain appa"

mkdir -p "$install_dir"
install -m 755 "$work/extract/appa" "$install_dir/appa"
version=$("$install_dir/appa" --version) ||
  fail "$install_dir/appa does not run on this system; Linux needs glibc 2.34 or newer"

printf 'Installed %s to %s\n' "$version" "$install_dir/appa"
case :$PATH: in
  *":$install_dir:"*) ;;
  *) printf 'Add %s to PATH to run appa by name.\n' "$install_dir" ;;
esac
printf 'Next: appa init claude-code\n'
