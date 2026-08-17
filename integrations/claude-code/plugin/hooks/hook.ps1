param(
    [switch]$SessionContext
)

# Hooks gate only sessions launched with APPA_GATE=1 (the clappa function).
# The guard reads the Claude Code process environment, fixed at launch, so a
# session cannot ungate itself. Without the variable the gate hook exits 0
# and -SessionContext prints beta-announcement.md — or setup-appa.md when
# the runtime binary is not installed: instructions for the model to offer
# and perform the install as a prompted task.
$gated = $env:APPA_GATE -eq "1"

$dataDir = if ($env:APPA_DATA_DIR) { $env:APPA_DATA_DIR } else { Join-Path $env:LOCALAPPDATA "appa" }
$configDir = if ($env:APPA_CONFIG_DIR) { $env:APPA_CONFIG_DIR } else { Join-Path $env:APPDATA "appa" }
$installDir = if ($env:APPA_INSTALL_DIR) { $env:APPA_INSTALL_DIR } else { Join-Path $dataDir "bin" }
$binary = Join-Path $installDir "appa-runtime-v2.exe"

if ($SessionContext) {
    if ($gated) {
        $document = "session-context.md"
    } elseif (Test-Path -LiteralPath $binary) {
        $document = "beta-announcement.md"
    } else {
        [Console]::Out.WriteLine("Install target for this machine: $binary")
        [Console]::Out.WriteLine("")
        $document = "setup-appa.md"
    }
    Get-Content -LiteralPath (Join-Path $PSScriptRoot $document) -Raw
    exit 0
}

if (-not $gated) {
    exit 0
}

$runtimeUrl = if ($env:APPA_RUNTIME_URL) {
    $env:APPA_RUNTIME_URL.TrimEnd("/")
} else {
    "http://127.0.0.1:8787"
}

function Test-RuntimeHealthy {
    try {
        (Invoke-RestMethod -Uri "$runtimeUrl/health" -TimeoutSec 2 -UseBasicParsing) -eq "ok"
    } catch {
        $false
    }
}

# SessionStart starts the installed runtime when nothing healthy answers,
# so a gated session works without any service setup. Installing the
# binary is not this script's job: an ungated session offers that as a
# prompted task.
function Start-RuntimeIfDown {
    if (Test-RuntimeHealthy) {
        return
    }
    if (-not (Test-Path -LiteralPath $binary)) {
        return
    }
    New-Item -ItemType Directory -Path $dataDir -Force | Out-Null
    Start-Process -FilePath $binary -WindowStyle Hidden -ArgumentList @(
        "--config", (Join-Path $configDir "appa.toml"),
        "--db", (Join-Path $dataDir "appa.db")
    ) | Out-Null
    for ($attempt = 0; $attempt -lt 15; $attempt++) {
        if (Test-RuntimeHealthy) {
            return
        }
        Start-Sleep -Seconds 1
    }
}

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
    $response = Invoke-WebRequest -Uri "$runtimeUrl/hook" -Method Post `
        -ContentType "application/json" -Body $payload -TimeoutSec 120 -UseBasicParsing
    [Console]::Out.Write([string]$response.Content)
} catch {
    $failure = $_.Exception.Message
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
