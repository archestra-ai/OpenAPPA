#!/bin/sh
# Make Claude Code's Linux sandbox run on a GitHub-hosted Ubuntu 24.04 runner,
# for scripts/claude-bash-selftest.sh. Throwaway runners only: it lifts
# machine-wide AppArmor hardening.
#
# The sandbox needs bubblewrap and socat. Ubuntu's bwrap-userns-restrict
# profile moves bwrap's children into unpriv_bwrap, which denies every
# capability, so Claude Code's seccomp helper cannot write
# /proc/self/setgroups in its nested user namespace
# (anthropics/claude-code#43454). The sysctl alone does not lift that, so the
# profile is unloaded and replaced by the one Claude Code's sandbox docs give.
set -eu

sudo apt-get update -qq
sudo apt-get install -y --no-install-recommends bubblewrap socat
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
