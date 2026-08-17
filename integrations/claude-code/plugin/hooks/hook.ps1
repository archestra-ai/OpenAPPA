param(
    [switch]$SessionContext
)

if ($SessionContext) {
    Get-Content -LiteralPath (Join-Path $PSScriptRoot "session-context.md") -Raw
    exit 0
}

$runtimeUrl = if ($env:APPA_RUNTIME_URL) {
    $env:APPA_RUNTIME_URL.TrimEnd("/")
} else {
    "http://127.0.0.1:8787"
}

try {
    $payload = [Console]::In.ReadToEnd()
    $response = Invoke-WebRequest -Uri "$runtimeUrl/hook" -Method Post `
        -ContentType "application/json" -Body $payload -TimeoutSec 120 -UseBasicParsing
    [Console]::Out.Write([string]$response.Content)
} catch {
    $failure = $_.Exception.Message
    $hookInput = $null
    try {
        $hookInput = $payload | ConvertFrom-Json
    } catch {
        $hookInput = $null
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
