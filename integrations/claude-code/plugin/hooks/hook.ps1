param(
    [switch]$SessionContext,
    [switch]$EnsureRuntime
)

# Hooks protect only sessions launched with APPA_GATE=1 (the clappa
# function). The guard reads the Claude Code process environment, fixed at
# launch, so a session cannot turn the protection off. Without the variable
# the protection hook exits 0 and -SessionContext prints nothing — or
# setup-appa.md when the runtime binary is not installed: instructions for
# the model to perform the install as a prompted task when the user asks.
$protected = $env:APPA_GATE -eq "1"

$dataDir = if ($env:APPA_DATA_DIR) { $env:APPA_DATA_DIR } else { Join-Path $env:LOCALAPPDATA "appa" }
$configDir = if ($env:APPA_CONFIG_DIR) { $env:APPA_CONFIG_DIR } else { Join-Path $env:APPDATA "appa" }
$installDir = if ($env:APPA_INSTALL_DIR) { $env:APPA_INSTALL_DIR } else { Join-Path $dataDir "bin" }
$binary = Join-Path $installDir "appa-runtime.exe"

if ($SessionContext) {
    if ($protected) {
        $document = "session-context.md"
    } elseif (Test-Path -LiteralPath $binary) {
        exit 0
    } else {
        [Console]::Out.WriteLine("Install target for this machine: $binary")
        [Console]::Out.WriteLine("Plugin files: $(Split-Path -Parent $PSScriptRoot)")
        [Console]::Out.WriteLine("")
        $document = "setup-appa.md"
    }
    Get-Content -LiteralPath (Join-Path $PSScriptRoot $document) -Raw
    exit 0
}

$runtimeUrl = if ($env:APPA_RUNTIME_URL) {
    $env:APPA_RUNTIME_URL.TrimEnd("/")
} else {
    "http://127.0.0.1:8787"
}

function Test-RuntimeHealthy {
    try {
        (Invoke-RestMethod -Uri "$runtimeUrl/health" -TimeoutSec 1 -UseBasicParsing) -eq "ok"
    } catch {
        $false
    }
}

# SessionStart starts the installed runtime when nothing healthy answers,
# so a protected session works without any service setup, and the last step
# of the install starts it through -EnsureRuntime. Installing the binary is
# not this script's job: an unprotected session performs that as a prompted
# task when the user asks.
function Start-RuntimeIfDown {
    if (Test-RuntimeHealthy) {
        return
    }
    if (-not (Test-Path -LiteralPath $binary)) {
        return
    }
    # The runtime writes the default policy on its first start and refuses
    # to start when it cannot. The policy and the database live in two
    # different directories on Windows, so both must exist first.
    New-Item -ItemType Directory -Path $configDir -Force | Out-Null
    New-Item -ItemType Directory -Path $dataDir -Force | Out-Null
    Start-Process -FilePath $binary -WindowStyle Hidden -ArgumentList @(
        "--config", (Join-Path $configDir "appa.toml"),
        "--db", (Join-Path $dataDir "appa.db")
    ) | Out-Null
    # A wall-clock budget, not a count of probes: one probe is instant
    # where the port refuses and costs the full deadline where it hangs.
    # The whole start must fit inside the timeout hooks.windows.json
    # declares for SessionStart.
    $deadline = (Get-Date).AddSeconds(20)
    while ((Get-Date) -lt $deadline) {
        if (Test-RuntimeHealthy) {
            return
        }
        Start-Sleep -Seconds 1
    }
}

# The last step of the install starts the runtime through this switch, so
# the install and every protected session start share one starter. It runs
# outside the protection guard, because the session that installs is not
# protected. It exits 0 only while /health answers, which is the proof the
# install reports.
if ($EnsureRuntime) {
    if (-not (Test-Path -LiteralPath $binary)) {
        [Console]::Error.WriteLine("appa protection: appa-runtime is not installed; expected at $binary")
        exit 1
    }
    Start-RuntimeIfDown
    if (Test-RuntimeHealthy) {
        exit 0
    }
    [Console]::Error.WriteLine("appa protection: runtime did not become healthy at $runtimeUrl. Its own error is the last line of $(Join-Path $dataDir 'runtime.stderr.log')")
    exit 1
}

if (-not $protected) {
    exit 0
}

# The hooks that report a finished turn. They decide nothing, so they
# never block, and they take the shorter deadline: a turn end waits on
# no evidence round trip.
$turnEnds = @("Stop", "StopFailure", "SubagentStop")

try {
    $payload = [Console]::In.ReadToEnd()
    $hookInput = $null
    try {
        $hookInput = $payload | ConvertFrom-Json
    } catch {
        $hookInput = $null
    }
    if ($null -ne $hookInput -and $hookInput.hook_event_name -eq "SessionStart") {
        Start-RuntimeIfDown
    }
    $timeout = if ($null -ne $hookInput -and $turnEnds -contains $hookInput.hook_event_name) { 30 } else { 120 }
    $response = Invoke-WebRequest -Uri "$runtimeUrl/hook" -Method Post `
        -ContentType "application/json" -Body $payload -TimeoutSec $timeout -UseBasicParsing
    [Console]::Out.Write([string]$response.Content)
} catch {
    $failure = $_.Exception.Message
    # A turn end decides nothing, and every blocking outcome on these
    # hooks means "do not stop", which would hold the actor in a turn it
    # has finished. A runtime that does not answer costs a call left
    # open, which the next turn end closes.
    if ($null -ne $hookInput -and $turnEnds -contains $hookInput.hook_event_name) {
        [Console]::Error.WriteLine("OpenAPPA runtime did not answer the turn end: $failure")
        exit 0
    }
    if ($null -ne $hookInput -and $hookInput.hook_event_name -eq "PostToolUse") {
        @{
            decision = "block"
            reason = "OpenAPPA runtime did not approve the tool result."
            hookSpecificOutput = @{
                hookEventName = "PostToolUse"
                updatedToolOutput = "OpenAPPA withheld this tool result because the runtime did not answer."
            }
        } | ConvertTo-Json -Compress -Depth 3
        exit 0
    }
    [Console]::Error.WriteLine("OpenAPPA runtime did not approve the hook: $failure")
    exit 2
}
