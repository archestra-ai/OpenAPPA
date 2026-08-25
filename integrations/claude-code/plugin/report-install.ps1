# Windows counterpart of report-install.sh. Keep the two in step: same event
# name, same properties, same refusals. A property added on one side and not
# the other silently splits the data by platform.
#
# The appa-setup skill runs this as its last step, after asking the user
# whether to send it. Nothing here fires unless a person said yes to a question
# they were shown. See report-install.sh for why the installer reports this and
# the runtime does not.
# Deliberately not Mandatory. A missing or empty value on a Mandatory
# parameter makes PowerShell stop and prompt for it — "Supply values for the
# following parameters" — which, at the end of an install with nothing on
# stdin, hangs and then dies instead of returning. The check below does the
# same job and keeps the promise the rest of this file makes: never block the
# install, always exit 0.
[CmdletBinding()]
param(
  [string]$Version,
  [string]$Os,
  [string]$Arch
)

# Failure here never becomes a problem for the install.
$ErrorActionPreference = 'SilentlyContinue'

# See report-install.sh: public, write-only project key, overridable by forks.
$posthogKey = if ($env:APPA_POSTHOG_KEY) { $env:APPA_POSTHOG_KEY } else { 'phc_v9AQ9LsFdiQoiPSR7GMW7qJYmazqzRFpad4D3KoidGB6' }
$posthogHost = if ($env:APPA_POSTHOG_HOST) { $env:APPA_POSTHOG_HOST } else { 'https://eu.i.posthog.com' }

# The non-interactive way to say no, for scripted or fleet installs.
if ($env:APPA_TELEMETRY -eq '0') { exit 0 }

# Written straight to stderr: $ErrorActionPreference above would swallow
# Write-Error, and a caller that passed the wrong arguments should still see why
# nothing was sent.
if (-not $Version -or -not $Os -or -not $Arch) {
  [Console]::Error.WriteLine('usage: report-install.ps1 -Version <version> -Os <os> -Arch <arch>')
  exit 0
}

# Constrained rather than trusted, matching the sh version: anything outside
# this set is dropped rather than escaped.
function Get-Sanitized([string]$value) {
  $clean = ($value -replace '[^A-Za-z0-9._-]', '')
  if ($clean.Length -gt 64) { $clean = $clean.Substring(0, 64) }
  return $clean
}

# Random per run and written nowhere, so two installs on one machine cannot be
# linked and no later event can be tied back to this one.
$payload = @{
  api_key     = $posthogKey
  event       = 'appa_installed'
  distinct_id = [guid]::NewGuid().ToString()
  properties  = @{
    '$process_person_profile' = $false
    appa_version              = Get-Sanitized $Version
    os                        = Get-Sanitized $Os
    arch                      = Get-Sanitized $Arch
    install_method            = 'claude-code-plugin'
  }
} | ConvertTo-Json -Compress -Depth 4

try {
  Invoke-RestMethod -Method Post -Uri "$posthogHost/i/v0/e/" `
    -ContentType 'application/json' -Body $payload -TimeoutSec 5 | Out-Null
} catch {
  # A slow or unreachable endpoint must not hold up the end of an install.
}

exit 0
