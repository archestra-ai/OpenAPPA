#!/bin/sh
# Stage the platform-independent APPA plugin bundle into <outdir>.
#
# The staged tree is a Claude Code marketplace root: the directory named by
# `claude plugin marketplace add`, and the layout `skills/appa-guide/SKILL.md`
# reads as <marketplace-root>. The release packages it as
# appa-plugin-<version>.tar.gz, and `appa init claude-code` accepts exactly the
# bytes whose SHA-256 its own build baked in.
#
# `plugin_bundle::validate_tree` is the single definition of the shape this must
# produce, and it runs against this script's real output in the init and
# rendered-hook tests, so a bundle that loses a required file fails CI here
# rather than at someone's install.
set -eu

if [ "$#" -ne 1 ]; then
  echo "usage: appa-stage-plugin-bundle.sh <outdir>" >&2
  exit 2
fi

outdir=$1
repo=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)

# The output directory must not exist: a single `mkdir` both reserves it and
# proves it is ours. Checking emptiness and then creating would leave a window
# in which anything under an attacker-writable parent such as /tmp could be
# swapped for a symlink, and every copy and chmod below would follow it.
if ! mkdir -- "$outdir" 2>/dev/null; then
  echo "appa-stage-plugin-bundle: cannot create $outdir: it must not exist yet, and its parent must" >&2
  exit 1
fi

# Everything is written through this working directory rather than through the
# pathname, so the destination stays the directory just created even if the name
# is replaced afterwards.
CDPATH='' cd -- "$outdir" || exit 1

cp -R -- "$repo/integrations/claude-code/.claude-plugin" ./.claude-plugin
cp -R -- "$repo/integrations/claude-code/plugin" ./plugin
cp -R -- "$repo/integrations/claude-code/examples" ./examples
cp -R -- "$repo/batteries" ./batteries
cp -- "$repo/integrations/claude-code/README.md" ./README.md
cp -- "$repo/integrations/claude-code/live-gate-check.py" ./live-gate-check.py

mkdir -p -- ./website/content/docs
cp -- "$repo/website/content/docs/contracts.md" ./website/content/docs/contracts.md

# Modes are applied by init when it materializes a deployment; these are for
# anyone who unpacks the archive by hand.
find . -type d -exec chmod 755 {} +
find . -type f -exec chmod 644 {} +
chmod 755 ./plugin/statusline.sh ./plugin/hooks/ensure-runtime.sh
