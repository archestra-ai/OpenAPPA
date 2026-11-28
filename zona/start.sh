#!/usr/bin/env bash
# zona's start hook: runs in the VM copy at every `zona start` and `zona env`.
# Safe to run twice.
set -euo pipefail

# First: every tool on PATH here is a mise shim, and mise refuses a config it
# hasn't been told to trust. Each worktree's copy is a new path.
/usr/local/bin/mise trust --yes --quiet mise.toml

# The runtime's working config (git-ignored, so it lives only in the VM copy
# and survives restarts): the Claude Code plugin's default policy. Its Claude
# annotators and sanitizers run `claude -p` in the VM: log in once with
# `zona run claude` (the login is shared by every worktree in the VM).
# Without a login they give no answer, and the policy refuses those calls.
# The batteries are the ones `appa plugin install claude-code` includes, from
# this checkout, in the package's order.
if [ ! -f appa.toml ]; then
  {
    echo "include = ["
    for battery in file-tracker coding claude-code; do
      echo "  \"./marketplace/batteries/$battery/appa.toml\","
    done
    echo "]"
    echo
    cat marketplace/plugins/claude-code/default.appa.toml
  } > appa.toml
  echo "Wrote appa.toml: the Claude Code plugin's default policy and batteries."
fi

cargo fetch --locked
