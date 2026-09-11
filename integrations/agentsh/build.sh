#!/usr/bin/env bash
set -euo pipefail

# Install into a new host-owned directory, never into the managed workspace.
destination=${1:?Usage: build.sh /absolute/new/backend-directory}
[[ $destination = /* && ! -e $destination ]] || { echo 'Destination must be new and absolute' >&2; exit 1; }
root=$(dirname "$(realpath "$0")")
source_dir=$(mktemp -d)
trap 'rm -rf "$source_dir"' EXIT
git -C "$source_dir" init -q
git -C "$source_dir" fetch -q --depth=1 https://github.com/canyonroad/agentsh.git 5173547e742e9337b574fd3450dc5d43ec421bee
git -C "$source_dir" checkout -q --detach FETCH_HEAD
git -C "$source_dir" apply "$root/fail-closed.patch"
mkdir "$destination"
(
  cd "$source_dir"
  CGO_ENABLED=1 go test ./internal/api -run 'TestSetupSeccompWrapper_(DisabledByConfig|WrapperNotFound|SocketPairFailure)$' -count=1
  CGO_ENABLED=1 go test ./internal/netmonitor/unix -run '^TestEmulationPath_ResolvePathAtFailureDenies$' -count=1
  CGO_ENABLED=1 go build -o "$destination/agentsh" ./cmd/agentsh
  CGO_ENABLED=1 go build -o "$destination/agentsh-unixwrap" ./cmd/agentsh-unixwrap
)
install -m 0644 "$root/run.py" "$destination/run.py"
install -m 0644 "$source_dir/LICENSE" "$destination/AGENTSH-LICENSE"
printf '%s\n' 'agentsh v0.20.5 with the OpenAPPA fail-closed patch; not an upstream release.' > "$destination/NOTICE"
echo "Built backend at $destination"
