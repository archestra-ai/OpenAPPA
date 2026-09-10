$lowerHalf = [char]0x2584
$fullBlock = [char]0x2588
$mascotTop = "$lowerHalf$fullBlock$lowerHalf$lowerHalf$lowerHalf$fullBlock$lowerHalf"
$mascotBottom = "$fullBlock$fullBlock$lowerHalf$fullBlock$lowerHalf$fullBlock$fullBlock"

try {
    [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
    if ($env:APPA_GATE -ne "1") {
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

    $escape = [char]0x1b
    $appa = "~$($status.appa_tokens) APPA tokens"
    $total = $statusInput.context_window.total_input_tokens
    if ($total -is [ValueType] -and $total -gt 0) {
        $percentage = (100 * [double]$status.appa_tokens / [double]$total).ToString(
            "0.0", [Globalization.CultureInfo]::InvariantCulture
        )
        $appa = "$appa ($percentage%)"
    }
    Write-Output "$mascotTop  trust:$($status.trust) $([char]0xb7) audience:$($status.audience) $([char]0xb7) $escape[2m$appa$escape[0m"
    Write-Output $mascotBottom
} catch {
    Write-Output $mascotTop
    Write-Output $mascotBottom
}

exit 0
