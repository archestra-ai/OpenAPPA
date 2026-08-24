# Windows counterpart of report-install.sh. Keep the two in step: same event
# name, same properties, same refusals. A property added on one side and not
# the other silently splits the data by platform.
#
# The appa-setup skill runs this as its last step, after asking the user
# whether to send it. Nothing here fires unless a person said yes to a question
# they were shown. See report-install.sh for why the installer reports this and
# the runtime does not.
[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)][string]$Version,
  [Parameter(Mandatory = $true)][string]$Os,
  [Parameter(Mandatory = $true)][string]$Arch
)

# Failure here never becomes a problem for the install.
$ErrorActionPreference = 'SilentlyContinue'

# See report-install.sh: public, write-only project key, overridable by forks.
$posthogKey = if ($env:APPA_POSTHOG_KEY) { $env:APPA_POSTHOG_KEY } else { 'phc_v9AQ9LsFdiQoiPSR7GMW7qJYmazqzRFpad4D3KoidGB6' }
$posthogHost = if ($env:APPA_POSTHOG_HOST) { $env:APPA_POSTHOG_HOST } else { 'https://eu.i.posthog.com' }

# The non-interactive way to say no, for scripted or fleet installs.
if ($env:APPA_TELEMETRY -eq '0') { exit 0 }

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
