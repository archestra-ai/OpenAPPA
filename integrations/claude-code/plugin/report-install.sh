#!/bin/sh
# Reports one anonymous "appa was installed" event, and only ever that.
#
# The appa-setup skill runs this as its last step, after asking the user
# whether to send it. That ordering is the whole design: nothing here fires
# unless a person said yes to a question they were shown, in a terminal they
# were watching, under the session's normal command approval.
#
# Why the installer and not the runtime. appa-runtime refuses to bind
# anything but loopback and makes no outbound call of its own; a product
# whose claim is deterministic control over where data goes should not quietly
# become an exception to it. The installer is a better fit anyway — it runs
# once per install, and it already knows the version, OS and architecture
# because it just used them to pick the release asset.
#
# What is sent: the release version, the OS and architecture from the asset
# name, and how it was installed. What is not sent: any identifier that
# survives this script. The distinct id below is random per run and stored
# nowhere, so two installs on one machine cannot be linked to each other, and
# no later event can be tied back to this one.
#
# Failure here is never the install's problem. Every path exits 0.
set -u

# Public, write-only project key for the OpenAPPA PostHog project. This is the
# same class of key that ships in openappa.com's JavaScript, where anyone can
# read it; it can add events and can read nothing back. Forks and self-hosted
# deployments override the two variables below rather than editing this file.
posthog_key=${APPA_POSTHOG_KEY:-phc_v9AQ9LsFdiQoiPSR7GMW7qJYmazqzRFpad4D3KoidGB6}
posthog_host=${APPA_POSTHOG_HOST:-https://eu.i.posthog.com}

# A second, non-interactive way to say no, for anyone scripting an install or
# setting it once for a whole fleet. Checked even though the skill has already
# asked, so that a machine configured to stay quiet stays quiet regardless of
# what any future caller of this script decides to do.
if [ "${APPA_TELEMETRY:-1}" = 0 ]; then
  exit 0
fi

if [ "$#" -lt 3 ]; then
  echo "usage: report-install.sh <version> <os> <arch>" >&2
  exit 0
fi

# uname output and the contents of version.txt reach us as arguments, so they
# are constrained rather than trusted: anything outside this set is dropped
# instead of escaped, which keeps the JSON below correct without a quoting
# routine written in sh.
sanitize() {
  printf '%s' "$1" | tr -cd 'A-Za-z0-9._-' | cut -c1-64
}

version=$(sanitize "$1")
os=$(sanitize "$2")
arch=$(sanitize "$3")

# Random per run, never written to disk. PostHog wants a distinct id on every
# event; this satisfies that without creating something that identifies the
# machine across time.
if command -v uuidgen >/dev/null 2>&1; then
  install_id=$(uuidgen | tr '[:upper:]' '[:lower:]')
else
  install_id=$(od -An -tx1 -N16 /dev/urandom 2>/dev/null | tr -d ' \n')
fi
[ -n "$install_id" ] || install_id="unknown-$$"

# $process_person_profile:false keeps this an event and nothing more — PostHog
# counts it without building a person record behind it, which is all a count of
# installs needs.
payload=$(cat <<JSON
{"api_key":"$posthog_key","event":"appa_installed","distinct_id":"$install_id","properties":{"\$process_person_profile":false,"appa_version":"$version","os":"$os","arch":"$arch","install_method":"claude-code-plugin"}}
JSON
)

# Short deadline and silenced output: a slow or unreachable endpoint must not
# hold up the end of an install, and a stack trace from curl is not something
# the person installing a security tool needs to read.
curl -sf -m 5 -X POST \
  -H 'Content-Type: application/json' \
  -d "$payload" \
  "$posthog_host/i/v0/e/" >/dev/null 2>&1 || true

exit 0
