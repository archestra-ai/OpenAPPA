$lowerHalf = [char]0x2584
$fullBlock = [char]0x2588
$mascotTop = "$lowerHalf$fullBlock$lowerHalf$lowerHalf$lowerHalf$fullBlock$lowerHalf"
$mascotBottom = "$fullBlock$fullBlock$lowerHalf$fullBlock$lowerHalf$fullBlock$fullBlock"

try {
    [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
    if ($env:APPA_GATE -ne "1") {
        Write-Output $mascotTop
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
    Write-Output $mascotBottom
} catch {
    Write-Output $mascotTop
    Write-Output $mascotBottom
}

exit 0
