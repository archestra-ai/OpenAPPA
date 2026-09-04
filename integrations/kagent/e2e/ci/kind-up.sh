#!/usr/bin/env bash
# Creates the kind cluster the live A2A matrix runs on and loads the three
# demo images into it. Run it after the image builds and before
# install.sh. It leaves the cluster as the current kubectl context.
#
#   ./kind-up.sh
#
# It keeps a cluster that already carries the name, so a second run
# costs one image load.
#
# Env, with defaults:
#   KIND_CLUSTER                 appa-e2e  the cluster name
#   APPA_E2E_IMAGE_TAG           ci        the tag the images carry
#   APPA_E2E_PRUNE_DAEMON_IMAGES 0         drop the daemon copies after
#                                          the load, for a short disk
set -euo pipefail

cluster=${KIND_CLUSTER:-appa-e2e}
tag=${APPA_E2E_IMAGE_TAG:-ci}
images=(
  "appa-kagent-quickstart:$tag"
  "appa-demo-tools:$tag"
  "appa-demo-mocks:$tag"
)

for image in "${images[@]}"; do
  if ! docker image inspect "$image" >/dev/null 2>&1; then
    echo "kind-up: the docker daemon carries no $image. Build the images first (README.md)." >&2
    exit 1
  fi
done

if kind get clusters 2>/dev/null | grep -qx -- "$cluster"; then
  echo "== kind cluster $cluster is up"
else
  echo "== kind create cluster $cluster"
  kind create cluster --name "$cluster" --wait 120s
fi

kubectl config use-context "kind-$cluster"

echo "== kind load ${images[*]}"
kind load docker-image "${images[@]}" --name "$cluster"

# The host now carries each image twice: once in the docker daemon and
# once in the cluster node. A CI runner needs that disk back.
if [ "${APPA_E2E_PRUNE_DAEMON_IMAGES:-0}" = 1 ]; then
  echo "== dropping the daemon copies"
  docker image rm "${images[@]}" >/dev/null
fi

kubectl get nodes
