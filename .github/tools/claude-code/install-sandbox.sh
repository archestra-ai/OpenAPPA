#!/bin/sh
# Make Claude Code's Linux sandbox run on a GitHub-hosted Ubuntu 24.04 runner,
# for scripts/claude-bash-selftest.sh and SafeBash's tests. Throwaway runners only: it lifts
# machine-wide AppArmor hardening.
#
# The sandbox needs bubblewrap and socat, and `srt` needs ripgrep too. Ubuntu
# 24.04's AppArmor default refuses bubblewrap the user namespace it needs
# ("setting up uid map: Permission denied"). On the hosted runner either the
# sysctl or the profile Claude Code's sandbox docs give lifts it; this script
# installs both, and unloads bwrap-userns-restrict where it is loaded, which
# on other Ubuntu hosts strips the capabilities Claude Code's seccomp helper
# needs (anthropics/claude-code#43454).
set -eu

sudo apt-get update -qq
sudo apt-get install -y --no-install-recommends bubblewrap socat ripgrep
if [ -f /etc/apparmor.d/bwrap-userns-restrict ]; then
  sudo apparmor_parser -R /etc/apparmor.d/bwrap-userns-restrict || true
fi
sudo tee /etc/apparmor.d/bwrap >/dev/null <<'PROFILE'
abi <abi/4.0>,
include <tunables/global>

profile bwrap /usr/bin/bwrap flags=(unconfined) {
  userns,
  include if exists <local/bwrap>
}
PROFILE
sudo apparmor_parser -r /etc/apparmor.d/bwrap
sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0
