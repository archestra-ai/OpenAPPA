$lowerHalf = [char]0x2584
$fullBlock = [char]0x2588
$mascotTop = "$lowerHalf$fullBlock$lowerHalf$lowerHalf$lowerHalf$fullBlock$lowerHalf"
$mascotBottom = "$fullBlock$fullBlock$lowerHalf$fullBlock$lowerHalf$fullBlock$fullBlock"

# Counts the policy's tools: every [[policy.tool]] entry, and the entries
# carrying more than the neutral annotation (bare name plus delta = {}).
# The policy is the file the runtime's status read names in policy_path;
# when no path arrived, the platform default under APPA_CONFIG_DIR's
# rules is the fallback. Mirrors statusline.sh; any parse failure
# returns nothing (fail open).
function Get-PolicyStats {
    param($LivePolicy)
    $policy = $LivePolicy
    if (-not $policy -or -not (Test-Path -LiteralPath $policy)) {
        $configDir = if ($env:APPA_CONFIG_DIR) { $env:APPA_CONFIG_DIR } else { Join-Path $env:APPDATA "appa" }
        $policy = Join-Path $configDir "appa.toml"
    }
    if (-not (Test-Path -LiteralPath $policy)) {
        return $null
    }
    try {
        $total = 0; $rules = 0; $inTool = $false; $tuned = $false
        foreach ($line in (Get-Content -LiteralPath $policy)) {
            if ($line -match '^\[\[policy\.tool\]\]') {
                if ($inTool -and $tuned) { $rules++ }
                $total++; $inTool = $true; $tuned = $false
                continue
            }
            if ($line -match '^\[policy\.tool\.') {
                if ($inTool) { $tuned = $true }
                continue
            }
            if ($line -match '^\[') {
                if ($inTool -and $tuned) { $rules++ }
                $inTool = $false
                continue
            }
            if ($inTool -and $line -match '^([A-Za-z_]+)[ \t]*=[ \t]*(.*)$') {
                $key = $Matches[1]
                if ($key -eq "name") { continue }
                if ($key -eq "delta" -and $Matches[2] -match '^\{[ \t]*\}[ \t]*$') { continue }
                $tuned = $true
            }
        }
        if ($inTool -and $tuned) { $rules++ }
        if ($total -gt 0) { "tools:$total rules:$rules" } else { $null }
    } catch {
        $null
    }
}

try {
    [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
    if ($env:APPA_GATE -ne "1") {
        Write-Output "$mascotTop  unprotected - run clappa to protect"
        Write-Output $mascotBottom
        exit 0
    }
    $statusInput = [Console]::In.ReadToEnd() | ConvertFrom-Json
    if (-not $statusInput.session_id) {
        throw "Status input has no session_id"
    }

    $runtimeUrl = if ($env:APPA_RUNTIME_URL) {
        $env:APPA_RUNTIME_URL.TrimEnd("/")
    } else {
        "http://127.0.0.1:8787"
    }
    $trajectory = [Uri]::EscapeDataString("cc:$($statusInput.session_id)")
    $status = Invoke-RestMethod -Uri "$runtimeUrl/status?trajectory=$trajectory" `
        -TimeoutSec 1 -UseBasicParsing
    if ($status.trust -isnot [string] -or $status.audience -isnot [string]) {
        throw "Runtime returned an invalid status"
    }

    Write-Output "$mascotTop  trust:$($status.trust)  audience:$($status.audience)"
    $livePolicy = $status.policy_path
} catch {
    Write-Output $mascotTop
}

$stats = Get-PolicyStats -LivePolicy $livePolicy
if ($stats) {
    Write-Output "$mascotBottom  $stats · /appa-tool-sync adds rules"
} else {
    Write-Output $mascotBottom
}

exit 0
