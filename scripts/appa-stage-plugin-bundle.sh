#!/bin/sh
# Stage the platform-independent APPA plugin bundle into <outdir>.
#
# The staged tree is a Claude Code marketplace root: the directory named by
# `claude plugin marketplace add`, and the layout `skills/appa-guide/SKILL.md`
# reads as <marketplace-root>. The release packages it as
# appa-plugin-<version>.tar.gz, and `appa init claude-code` accepts exactly the
# bytes whose SHA-256 its own build baked in.
set -eu

if [ "$#" -ne 1 ]; then
  echo "usage: appa-stage-plugin-bundle.sh <outdir>" >&2
  exit 2
fi

outdir=$1
repo=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)

if [ -e "$outdir" ] && [ -n "$(ls -A -- "$outdir" 2>/dev/null)" ]; then
  echo "appa-stage-plugin-bundle: $outdir exists and is not empty" >&2
  exit 1
fi

mkdir -p -- "$outdir"
outdir=$(CDPATH='' cd -- "$outdir" && pwd)

cp -R -- "$repo/integrations/claude-code/.claude-plugin" "$outdir/.claude-plugin"
cp -R -- "$repo/integrations/claude-code/plugin" "$outdir/plugin"
cp -R -- "$repo/integrations/claude-code/examples" "$outdir/examples"
cp -R -- "$repo/batteries" "$outdir/batteries"
cp -- "$repo/integrations/claude-code/README.md" "$outdir/README.md"
cp -- "$repo/integrations/claude-code/live-gate-check.py" "$outdir/live-gate-check.py"

mkdir -p -- "$outdir/website/content/docs"
cp -- "$repo/website/content/docs/contracts.md" "$outdir/website/content/docs/contracts.md"

# Modes are applied by init when it materializes a deployment; these are for
# anyone who unpacks the archive by hand.
find "$outdir" -type d -exec chmod 755 {} +
find "$outdir" -type f -exec chmod 644 {} +
chmod 755 "$outdir/plugin/statusline.sh" "$outdir/plugin/hooks/ensure-runtime.sh"

for required in \
  .claude-plugin/marketplace.json \
  plugin/.claude-plugin/plugin.json \
  plugin/hooks/hooks.json \
  plugin/hooks/hooks.windows.json \
  batteries/README.md \
  website/content/docs/contracts.md
do
  if [ ! -f "$outdir/$required" ]; then
    echo "appa-stage-plugin-bundle: staged tree is missing $required" >&2
    exit 1
  fi
done
