#!/bin/sh
# Stage the platform-independent APPA plugin bundle into <outdir>.
#
# The staged tree is a Claude Code marketplace root: the directory named by
# `claude plugin marketplace add`, and the layout `skills/appa-guide/SKILL.md`
# reads as <marketplace-root>. The release packages it as
# appa-plugin-<version>.tar.gz, and `appa init claude-code` accepts exactly the
# bytes whose SHA-256 its own build baked in.
#
# `plugin_layout::REPOSITORY_MAPPINGS` is the single definition of what this
# copies, and a runtime unit test digests this script's real output against the
# mapping's, so a release bundle that drifts from what init stages fails CI here
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
# appa-guide is host-neutral source. Materialize it at Claude's required
# plugin path rather than keeping a second source copy under claude-code/.
mkdir -p -- ./plugin/skills
cp -R -- "$repo/integrations/appa-guide" ./plugin/skills/appa-guide
# Claude loads SKILL.md before any gated tool call. Inline its host reference
# so the guide can bootstrap even when the current policy refuses `Read`.
printf '\n\n' >> ./plugin/skills/appa-guide/SKILL.md
cat ./plugin/skills/appa-guide/references/claude-code.md >> ./plugin/skills/appa-guide/SKILL.md
cp -R -- "$repo/integrations/claude-code/examples" ./examples
cp -R -- "$repo/batteries" ./batteries
cp -- "$repo/integrations/claude-code/README.md" ./README.md
cp -- "$repo/integrations/claude-code/live-gate-check.py" ./live-gate-check.py

mkdir -p -- ./website/content/docs
cp -- "$repo/website/content/docs/contracts.md" ./website/content/docs/contracts.md

# Generated Python caches are not plugin source. A developer may have them in a
# checkout, while a GitHub source archive and a clean release runner never do;
# excluding them keeps all three staging paths byte-identical.
find . -type d -name __pycache__ -prune -exec rm -rf -- {} +
find . -type f \( -name '*.pyc' -o -name '*.pyo' \) -delete

# Modes are applied by init when it materializes a deployment; these are for
# anyone who unpacks the archive by hand.
find . -type d -exec chmod 755 {} +
find . -type f -exec chmod 644 {} +
chmod 755 ./plugin/statusline.sh ./plugin/hooks/ensure-runtime.sh
