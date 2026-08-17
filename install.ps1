[CmdletBinding()]
param(
    [switch]$Uninstall
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

function Get-EnvironmentValue {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string]$Default
    )

    $value = [Environment]::GetEnvironmentVariable($Name)
    if ([string]::IsNullOrWhiteSpace($value)) {
        return $Default
    }
    return $value
}

function Invoke-AppaTaskStop {
    $task = Get-ScheduledTask -TaskName $script:TaskName -ErrorAction SilentlyContinue
    if ($null -ne $task) {
        Stop-ScheduledTask -TaskName $script:TaskName -ErrorAction Stop
        for ($attempt = 0; $attempt -lt 20; $attempt++) {
            if ((Get-ScheduledTask -TaskName $script:TaskName).State -ne "Running") {
                return
            }
            Start-Sleep -Milliseconds 100
        }
        throw "Could not stop the installed Scheduled Task"
    }
}

function Test-AppaPortInUse {
    $client = [Net.Sockets.TcpClient]::new()
    try {
        $connection = $client.BeginConnect("127.0.0.1", 8787, $null, $null)
        if (-not $connection.AsyncWaitHandle.WaitOne(1000)) {
            return $false
        }
        $client.EndConnect($connection)
        return $client.Connected
    } catch {
        return $false
    } finally {
        $client.Dispose()
    }
}

$repository = Get-EnvironmentValue -Name "APPA_REPOSITORY" -Default "archestra-ai/OpenAPPA"
$localData = [Environment]::GetFolderPath([Environment+SpecialFolder]::LocalApplicationData)
$roamingData = [Environment]::GetFolderPath([Environment+SpecialFolder]::ApplicationData)
$dataDir = Get-EnvironmentValue -Name "APPA_DATA_DIR" -Default (Join-Path $localData "appa")
$configDir = Get-EnvironmentValue -Name "APPA_CONFIG_DIR" -Default (Join-Path $roamingData "appa")
$installDir = Get-EnvironmentValue -Name "APPA_INSTALL_DIR" -Default (Join-Path $dataDir "bin")
$skipServiceValue = Get-EnvironmentValue -Name "APPA_SKIP_SERVICE" -Default "0"
if ($skipServiceValue -notin @("0", "1")) {
    throw "APPA_SKIP_SERVICE must be 0 or 1"
}
$skipService = $skipServiceValue -eq "1"
$script:TaskName = "appa-runtime-v2"
$binary = Join-Path $installDir "appa-runtime-v2.exe"
$configFile = Join-Path $configDir "appa.toml"
$dbFile = Join-Path $dataDir "appa.db"
$pluginDir = Join-Path $dataDir "claude-code"
$instanceId = [Guid]::NewGuid().ToString("N")
foreach ($directory in @($installDir, $configDir, $dataDir)) {
    $isDriveAbsolute = $directory -match '^[A-Za-z]:[\\/]'
    $isUncAbsolute = $directory -match '^\\\\[^\\/]+[\\/][^\\/]+'
    if (-not $isDriveAbsolute -and -not $isUncAbsolute) {
        throw "Installation directories must be absolute: $directory"
    }
}

if ($Uninstall) {
    Invoke-AppaTaskStop
    if ($null -ne (Get-ScheduledTask -TaskName $script:TaskName -ErrorAction SilentlyContinue)) {
        Unregister-ScheduledTask -TaskName $script:TaskName -Confirm:$false -ErrorAction Stop
    }
    if (Test-Path -LiteralPath $binary) {
        Remove-Item -LiteralPath $binary -Force -ErrorAction Stop
    }
    if (Test-Path -LiteralPath $pluginDir) {
        Remove-Item -LiteralPath $pluginDir -Recurse -Force -ErrorAction Stop
    }
    if (Test-Path -LiteralPath $binary) { throw "Could not remove $binary" }
    if (Test-Path -LiteralPath $pluginDir) { throw "Could not remove $pluginDir" }
    Write-Output "Removed appa-runtime-v2 and Claude Code integration files."
    Write-Output "Preserved policy: $configFile"
    Write-Output "Preserved database: $dbFile"
    return
}

$architecture = if ($env:PROCESSOR_ARCHITEW6432) {
    $env:PROCESSOR_ARCHITEW6432
} else {
    $env:PROCESSOR_ARCHITECTURE
}
switch ($architecture.ToUpperInvariant()) {
    "AMD64" { $target = "x86_64-pc-windows-msvc" }
    "ARM64" { $target = "aarch64-pc-windows-msvc" }
    default { throw "Unsupported Windows architecture: $architecture" }
}
$archive = "appa-runtime-v2-$target.zip"

$requestedVersion = [Environment]::GetEnvironmentVariable("APPA_VERSION")
if ($null -eq $requestedVersion) {
    $requestedVersion = ""
}
$requestedVersion = $requestedVersion.TrimStart("v")
if ($requestedVersion -and $requestedVersion -notmatch '^[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.-]+)?$') {
    throw "Invalid APPA_VERSION: $requestedVersion"
}

$releaseTag = if ($requestedVersion) { "v$requestedVersion" } else { "" }
$downloadBaseOverride = [Environment]::GetEnvironmentVariable("APPA_DOWNLOAD_BASE")
if ($downloadBaseOverride) {
    $assetBase = $downloadBaseOverride.TrimEnd("/")
    $overrideUri = [Uri]$assetBase
    $allowedOverride = $overrideUri.IsAbsoluteUri -and (
        $overrideUri.Scheme -eq "https" -or
        $overrideUri.Scheme -eq "file" -or
        ($overrideUri.Scheme -eq "http" -and $overrideUri.IsLoopback)
    )
    if (-not $allowedOverride) {
        throw "APPA_DOWNLOAD_BASE must use HTTPS, file, or loopback HTTP"
    }
} elseif ($releaseTag) {
    $assetBase = "https://github.com/$repository/releases/download/$releaseTag"
} else {
    $assetBase = "https://github.com/$repository/releases/latest/download"
}

$tempDir = Join-Path ([IO.Path]::GetTempPath()) "appa-install-$([Guid]::NewGuid())"
New-Item -ItemType Directory -Path $tempDir | Out-Null
$binaryNew = "$binary.new"
$binaryOld = "$binary.old"
$pluginNew = "$pluginDir.new"
$pluginOld = "$pluginDir.old"
$hadBinary = $false
$hadPlugin = $false
$binaryInstalled = $false
$pluginInstalled = $false
$oldTaskXml = $null
$oldTaskWasRunning = $false
$taskTouched = $false
try {
    $script:UseGh = $false
    if (-not $downloadBaseOverride -and $null -ne (Get-Command gh -ErrorAction SilentlyContinue)) {
        & gh auth status --hostname github.com *> $null
        $script:UseGh = $LASTEXITCODE -eq 0
    }

    function Get-ReleaseAsset {
        param([string]$Name)

        $destination = Join-Path $tempDir $Name
        Remove-Item -LiteralPath $destination -Force -ErrorAction SilentlyContinue
        if ($script:UseGh) {
            $arguments = @("release", "download")
            if ($releaseTag) {
                $arguments += $releaseTag
            }
            $arguments += @("--repo", $repository, "--pattern", $Name, "--dir", $tempDir, "--clobber")
            & gh @arguments
            if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $destination)) {
                throw "Release asset not found: $Name"
            }
            return $destination
        }

        try {
            Invoke-WebRequest -Uri "$assetBase/$Name" -OutFile $destination -UseBasicParsing
        } catch {
            throw "Could not download $Name. Authenticate gh for a private repository. $($_.Exception.Message)"
        }
        return $destination
    }

    $versionPath = Get-ReleaseAsset -Name "version.txt"
    $version = (Get-Content -LiteralPath $versionPath -Raw).Trim()
    if ($version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.-]+)?$') {
        throw "Release contains an invalid version.txt"
    }
    if ($requestedVersion -and $version -ne $requestedVersion) {
        throw "Requested $requestedVersion but release reports $version"
    }

    $checksumsPath = Get-ReleaseAsset -Name "SHA256SUMS"
    $archivePath = Get-ReleaseAsset -Name $archive
    $escapedArchive = [Regex]::Escape($archive)
    $checksumLines = @(Get-Content -LiteralPath $checksumsPath | Where-Object {
        $_ -match "^([0-9A-Fa-f]{64}) [ *]$escapedArchive$"
    })
    if ($checksumLines.Count -ne 1) {
        throw "SHA256SUMS must name $archive exactly once"
    }
    $expectedChecksum = ($checksumLines[0] -split " ", 2)[0].ToLowerInvariant()
    $actualChecksum = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualChecksum -ne $expectedChecksum) {
        throw "Checksum mismatch for $archive"
    }

    $extractDir = Join-Path $tempDir "extract"
    Expand-Archive -LiteralPath $archivePath -DestinationPath $extractDir
    $sourceBinary = Join-Path $extractDir "appa-runtime-v2.exe"
    $sourcePlugin = Join-Path $extractDir "claude-code"
    if (-not (Test-Path -LiteralPath $sourceBinary -PathType Leaf)) {
        throw "$archive does not contain appa-runtime-v2.exe"
    }
    if (-not (Test-Path -LiteralPath (Join-Path $sourcePlugin ".claude-plugin\marketplace.json") -PathType Leaf)) {
        throw "$archive does not contain the Claude Code marketplace"
    }
    if (-not (Test-Path -LiteralPath (Join-Path $sourcePlugin "plugin\hooks\hooks.json") -PathType Leaf)) {
        throw "$archive does not contain the Claude Code plugin"
    }
    if (-not (Test-Path -LiteralPath (Join-Path $sourcePlugin "plugin\.claude-plugin\plugin.json") -PathType Leaf)) {
        throw "$archive does not contain the Claude Code plugin manifest"
    }
    if (-not (Test-Path -LiteralPath (Join-Path $sourcePlugin "plugin\.mcp.json") -PathType Leaf)) {
        throw "$archive does not contain the Claude Code MCP configuration"
    }
    if (-not (Test-Path -LiteralPath (Join-Path $sourcePlugin "plugin\hooks\session-context.md") -PathType Leaf)) {
        throw "$archive does not contain the Claude Code session context"
    }
    if (-not (Test-Path -LiteralPath (Join-Path $sourcePlugin "plugin\skills\appa-tool-sync\SKILL.md") -PathType Leaf)) {
        throw "$archive does not contain the Claude Code tool-sync skill"
    }
    if (-not (Test-Path -LiteralPath (Join-Path $sourcePlugin "plugin\statusline.sh") -PathType Leaf)) {
        throw "$archive does not contain the Claude Code statusline"
    }

    $reportedVersion = (& $sourceBinary --version | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $reportedVersion -ne "appa-runtime-v2 $version") {
        throw "Binary reports '$reportedVersion', expected 'appa-runtime-v2 $version'"
    }
    & $sourceBinary --instance-id installer-probe --version *> $null
    $supportsInstanceId = $LASTEXITCODE -eq 0

    if (-not $skipService) {
        $oldTask = Get-ScheduledTask -TaskName $script:TaskName -ErrorAction SilentlyContinue
        if ($null -ne $oldTask) {
            $oldTaskXml = Export-ScheduledTask -TaskName $script:TaskName
            $oldTaskWasRunning = $oldTask.State -eq "Running"
        }
        $taskTouched = $true
        Invoke-AppaTaskStop
        if (-not $supportsInstanceId -and (Test-AppaPortInUse)) {
            throw "Port 8787 is already in use by a process outside the installed Scheduled Task"
        }
    }
    New-Item -ItemType Directory -Force -Path $installDir, $configDir, $dataDir | Out-Null

    Remove-Item -LiteralPath $binaryNew, $binaryOld -Force -ErrorAction SilentlyContinue
    Copy-Item -LiteralPath $sourceBinary -Destination $binaryNew
    if (Test-Path -LiteralPath $binary) {
        Move-Item -LiteralPath $binary -Destination $binaryOld
        $hadBinary = $true
    }
    Move-Item -LiteralPath $binaryNew -Destination $binary
    $binaryInstalled = $true

    Remove-Item -LiteralPath $pluginNew, $pluginOld -Recurse -Force -ErrorAction SilentlyContinue
    Copy-Item -LiteralPath $sourcePlugin -Destination $pluginNew -Recurse
    if (Test-Path -LiteralPath $pluginDir) {
        Move-Item -LiteralPath $pluginDir -Destination $pluginOld
        $hadPlugin = $true
    }
    Move-Item -LiteralPath $pluginNew -Destination $pluginDir
    $pluginInstalled = $true

    if (-not $skipService) {
        $currentUser = [Security.Principal.WindowsIdentity]::GetCurrent().Name
        $arguments = "--config `"$configFile`" --db `"$dbFile`""
        if ($supportsInstanceId) {
            $arguments += " --instance-id `"$instanceId`""
        }
        $action = New-ScheduledTaskAction -Execute $binary -Argument $arguments -WorkingDirectory $dataDir
        $trigger = New-ScheduledTaskTrigger -AtLogOn -User $currentUser
        $principal = New-ScheduledTaskPrincipal -UserId $currentUser -LogonType Interactive -RunLevel Limited
        $settings = New-ScheduledTaskSettingsSet -StartWhenAvailable -MultipleInstances IgnoreNew `
            -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
            -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1) `
            -ExecutionTimeLimit ([TimeSpan]::Zero)
        $task = New-ScheduledTask -Action $action -Trigger $trigger -Principal $principal -Settings $settings
        Register-ScheduledTask -TaskName $script:TaskName -InputObject $task -Force | Out-Null
        Start-ScheduledTask -TaskName $script:TaskName

        $healthy = $false
        for ($attempt = 0; $attempt -lt 30; $attempt++) {
            try {
                $health = Invoke-WebRequest -Uri "http://127.0.0.1:8787/health" -UseBasicParsing -TimeoutSec 1
                $returnedInstanceId = $health.Headers["X-Appa-Instance-Id"]
                $healthIsOurs = -not $supportsInstanceId -or $returnedInstanceId -eq $instanceId
                if ($health.Content.Trim() -eq "ok" -and $healthIsOurs) {
                    if ((Get-ScheduledTask -TaskName $script:TaskName).State -ne "Running") {
                        throw "Runtime endpoint is healthy, but the installed Scheduled Task is not running"
                    }
                    $healthy = $true
                    break
                }
            } catch {
                Start-Sleep -Seconds 1
            }
        }
        if (-not $healthy) {
            throw "Runtime did not become healthy at http://127.0.0.1:8787/health"
        }
    }

    Remove-Item -LiteralPath $binaryOld -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $pluginOld -Recurse -Force -ErrorAction SilentlyContinue

    Write-Output "Installed appa-runtime-v2 $version."
    Write-Output "Runtime: $binary"
    Write-Output "Policy: $configFile"
    Write-Output "Database: $dbFile"
    Write-Output "Claude plugin: $(Join-Path $pluginDir 'plugin')"
    if ($skipService) {
        Write-Output "Login startup skipped. Start manually:"
        Write-Output "  `"$binary`" --config `"$configFile`" --db `"$dbFile`""
    }
    if ($null -ne (Get-Command claude -ErrorAction SilentlyContinue)) {
        Write-Output "Claude Code detected, but OpenAPPA hooks require a POSIX shell."
    } else {
        Write-Output "Claude Code not found."
    }
    Write-Output "For Claude Code gating on Windows, install OpenAPPA inside WSL."
} catch {
    $installFailure = $_
    try {
        if ($taskTouched) {
            $currentTask = Get-ScheduledTask -TaskName $script:TaskName -ErrorAction SilentlyContinue
            if ($null -ne $currentTask) {
                Invoke-AppaTaskStop
                Unregister-ScheduledTask -TaskName $script:TaskName -Confirm:$false -ErrorAction Stop
            }
        }
        if ($pluginInstalled -and (Test-Path -LiteralPath $pluginDir)) {
            Remove-Item -LiteralPath $pluginDir -Recurse -Force
        }
        Remove-Item -LiteralPath $pluginNew -Recurse -Force -ErrorAction SilentlyContinue
        if ($hadPlugin -and (Test-Path -LiteralPath $pluginOld)) {
            Move-Item -LiteralPath $pluginOld -Destination $pluginDir
        }
        if ($binaryInstalled -and (Test-Path -LiteralPath $binary)) {
            Remove-Item -LiteralPath $binary -Force
        }
        Remove-Item -LiteralPath $binaryNew -Force -ErrorAction SilentlyContinue
        if ($hadBinary -and (Test-Path -LiteralPath $binaryOld)) {
            Move-Item -LiteralPath $binaryOld -Destination $binary
        }
        if ($taskTouched -and $null -ne $oldTaskXml) {
            Register-ScheduledTask -TaskName $script:TaskName -Xml $oldTaskXml -Force | Out-Null
            if ($oldTaskWasRunning) {
                Start-ScheduledTask -TaskName $script:TaskName
            }
        }
    } catch {
        Write-Warning "Installation failed and rollback was incomplete: $($_.Exception.Message)"
    }
    throw $installFailure
} finally {
    Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
}
