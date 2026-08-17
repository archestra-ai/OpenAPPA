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
    param([string]$Name = $script:TaskName)

    $task = Get-ScheduledTask -TaskName $Name -ErrorAction SilentlyContinue
    if ($null -ne $task) {
        Stop-ScheduledTask -TaskName $Name -ErrorAction Stop
        $stopped = $false
        for ($attempt = 0; $attempt -lt 20; $attempt++) {
            if ((Get-ScheduledTask -TaskName $Name).State -ne "Running") {
                $stopped = $true
                break
            }
            Start-Sleep -Milliseconds 100
        }
        if (-not $stopped) {
            throw "Could not stop the installed Scheduled Task"
        }
    }
    Stop-AppaRuntimeProcess
}

function Stop-AppaRuntimeProcess {
    [CmdletBinding(SupportsShouldProcess)]
    param()

    $runtimePids = @()
    $runtimePid = 0
    if ((Test-Path -LiteralPath $script:PidFile -PathType Leaf) -and
        [int]::TryParse((Get-Content -LiteralPath $script:PidFile -Raw).Trim(), [ref]$runtimePid)) {
        $pidProcess = Get-Process -Id $runtimePid -ErrorAction SilentlyContinue
        if ($null -ne $pidProcess -and $pidProcess.Path -ieq $script:Binary) {
            $runtimePids += $runtimePid
        }
    }
    if ($runtimePids.Count -eq 0) {
        $runtimePids = @(Get-CimInstance Win32_Process -Filter "Name = 'appa-runtime-v2.exe'" |
            Where-Object { $_.ExecutablePath -ieq $script:Binary } |
            Select-Object -ExpandProperty ProcessId)
    }
    foreach ($processId in $runtimePids) {
        $process = Get-Process -Id $processId -ErrorAction SilentlyContinue
        if ($null -ne $process -and $process.Path -ieq $script:Binary -and
            $PSCmdlet.ShouldProcess("runtime process $processId", "Stop")) {
            Stop-Process -Id $processId -Force -ErrorAction Stop
            for ($attempt = 0; $attempt -lt 50; $attempt++) {
                if ($null -eq (Get-Process -Id $processId -ErrorAction SilentlyContinue)) {
                    break
                }
                Start-Sleep -Milliseconds 100
            }
            if ($null -ne (Get-Process -Id $processId -ErrorAction SilentlyContinue)) {
                throw "Could not stop installed runtime process $processId"
            }
        }
    }
    Remove-Item -LiteralPath $script:PidFile -Force -ErrorAction SilentlyContinue
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
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$script:CurrentUser = $identity.Name
$script:CurrentSid = $identity.User.Value
$script:TaskName = "appa-runtime-v2-$($script:CurrentSid)"
$script:LegacyTaskName = "appa-runtime-v2"
$script:Binary = Join-Path $installDir "appa-runtime-v2.exe"
$binary = $script:Binary
$configFile = Join-Path $configDir "appa.toml"
$dbFile = Join-Path $dataDir "appa.db"
$pluginDir = Join-Path $dataDir "claude-code"
$launcher = Join-Path $dataDir "appa-runtime-v2.vbs"
$script:PidFile = Join-Path $dataDir "appa-runtime-v2.pid"
$instanceId = [Guid]::NewGuid().ToString("N")
foreach ($directory in @($installDir, $configDir, $dataDir)) {
    $isDriveAbsolute = $directory -match '^[A-Za-z]:[\\/]'
    $isUncAbsolute = $directory -match '^\\\\[^\\/]+[\\/][^\\/]+'
    if (-not $isDriveAbsolute -and -not $isUncAbsolute) {
        throw "Installation directories must be absolute: $directory"
    }
}

if ($Uninstall) {
    foreach ($taskName in @($script:TaskName, $script:LegacyTaskName)) {
        $task = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
        if ($null -eq $task) {
            continue
        }
        $isOwned = $task.Principal.UserId -in @($script:CurrentUser, $script:CurrentSid)
        if ($taskName -eq $script:LegacyTaskName -and -not $isOwned) {
            continue
        }
        Invoke-AppaTaskStop -Name $taskName
        Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction Stop
    }
    if (Test-Path -LiteralPath $binary) {
        Remove-Item -LiteralPath $binary -Force -ErrorAction Stop
    }
    if (Test-Path -LiteralPath $pluginDir) {
        Remove-Item -LiteralPath $pluginDir -Recurse -Force -ErrorAction Stop
    }
    if (Test-Path -LiteralPath $launcher) {
        Remove-Item -LiteralPath $launcher -Force -ErrorAction Stop
    }
    Remove-Item -LiteralPath $script:PidFile -Force -ErrorAction SilentlyContinue
    if (Test-Path -LiteralPath $binary) { throw "Could not remove $binary" }
    if (Test-Path -LiteralPath $pluginDir) { throw "Could not remove $pluginDir" }
    if (Test-Path -LiteralPath $launcher) { throw "Could not remove $launcher" }
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
$legacyTaskXml = $null
$legacyTaskWasRunning = $false
$legacyTaskTouched = $false
$oldLauncher = $null
$launcherTouched = $false
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
    if (-not (Test-Path -LiteralPath (Join-Path $sourcePlugin "plugin\statusline.ps1") -PathType Leaf)) {
        throw "$archive does not contain the Windows Claude Code statusline"
    }
    if (-not (Test-Path -LiteralPath (Join-Path $sourcePlugin "plugin\hooks\hooks.windows.json") -PathType Leaf)) {
        throw "$archive does not contain the Windows Claude Code hooks"
    }
    if (-not (Test-Path -LiteralPath (Join-Path $sourcePlugin "plugin\hooks\hook.ps1") -PathType Leaf)) {
        throw "$archive does not contain the Windows Claude Code hook adapter"
    }

    $reportedVersion = (& $sourceBinary --version | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $reportedVersion -ne "appa-runtime-v2 $version") {
        throw "Binary reports '$reportedVersion', expected 'appa-runtime-v2 $version'"
    }
    & $sourceBinary --instance-id installer-probe --version *> $null
    $supportsInstanceId = $LASTEXITCODE -eq 0

    if (-not $skipService) {
        $legacyTask = Get-ScheduledTask -TaskName $script:LegacyTaskName -ErrorAction SilentlyContinue
        if ($null -ne $legacyTask -and
            $legacyTask.Principal.UserId -in @($script:CurrentUser, $script:CurrentSid)) {
            $legacyTaskXml = Export-ScheduledTask -TaskName $script:LegacyTaskName
            $legacyTaskWasRunning = $legacyTask.State -eq "Running"
            $legacyTaskTouched = $true
            Invoke-AppaTaskStop -Name $script:LegacyTaskName
            Unregister-ScheduledTask -TaskName $script:LegacyTaskName -Confirm:$false -ErrorAction Stop
        }
        $oldTask = Get-ScheduledTask -TaskName $script:TaskName -ErrorAction SilentlyContinue
        if ($null -ne $oldTask) {
            $oldTaskXml = Export-ScheduledTask -TaskName $script:TaskName
            $oldTaskWasRunning = $oldTask.State -eq "Running"
        }
        if (Test-Path -LiteralPath $launcher) {
            $oldLauncher = Get-Content -LiteralPath $launcher -Raw
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
    Copy-Item -LiteralPath (Join-Path $pluginNew "plugin\hooks\hooks.windows.json") `
        -Destination (Join-Path $pluginNew "plugin\hooks\hooks.json") -Force
    $hookConfigPath = Join-Path $pluginNew "plugin\hooks\hooks.json"
    $hookConfig = Get-Content -LiteralPath $hookConfigPath -Raw | ConvertFrom-Json
    $powershellPath = (Join-Path $env:SystemRoot "System32\WindowsPowerShell\v1.0\powershell.exe").Replace('\', '/')
    foreach ($event in $hookConfig.hooks.PSObject.Properties) {
        foreach ($group in $event.Value) {
            foreach ($handler in $group.hooks) {
                if ($handler.type -eq "command" -and $handler.command -eq "powershell.exe") {
                    $handler.command = $powershellPath
                }
            }
        }
    }
    $hookJson = $hookConfig | ConvertTo-Json -Depth 10
    $utf8NoBom = New-Object System.Text.UTF8Encoding($false)
    [IO.File]::WriteAllText($hookConfigPath, $hookJson, $utf8NoBom)
    if (Test-Path -LiteralPath $pluginDir) {
        Move-Item -LiteralPath $pluginDir -Destination $pluginOld
        $hadPlugin = $true
    }
    Move-Item -LiteralPath $pluginNew -Destination $pluginDir
    $pluginInstalled = $true

    if (-not $skipService) {
        $currentUser = [Security.Principal.WindowsIdentity]::GetCurrent().Name
        $runtimeCommand = "`"$binary`" --config `"$configFile`" --db `"$dbFile`""
        if ($supportsInstanceId) {
            $runtimeCommand += " --instance-id `"$instanceId`""
        }
        $vbsCommand = $runtimeCommand.Replace('"', '""')
        $vbsPidFile = $script:PidFile.Replace('"', '""')
        $launcherContent = @"
Option Explicit
Dim startupConfig, processConfig, processId, result, fileSystem, pidHandle, processes
Set startupConfig = GetObject("winmgmts:{impersonationLevel=impersonate}!\\.\root\cimv2:Win32_ProcessStartup").SpawnInstance_
startupConfig.ShowWindow = 0
Set processConfig = GetObject("winmgmts:{impersonationLevel=impersonate}!\\.\root\cimv2:Win32_Process")
result = processConfig.Create("$vbsCommand", Null, startupConfig, processId)
If result <> 0 Then WScript.Quit result
Set fileSystem = CreateObject("Scripting.FileSystemObject")
Set pidHandle = fileSystem.CreateTextFile("$vbsPidFile", True)
pidHandle.Write CStr(processId)
pidHandle.Close
Do
  WScript.Sleep 1000
  Set processes = GetObject("winmgmts:{impersonationLevel=impersonate}!\\.\root\cimv2").ExecQuery("SELECT ProcessId FROM Win32_Process WHERE ProcessId = " & processId)
Loop While processes.Count > 0
If fileSystem.FileExists("$vbsPidFile") Then fileSystem.DeleteFile "$vbsPidFile", True
WScript.Quit 1
"@
        Set-Content -LiteralPath $launcher -Value $launcherContent -Encoding Unicode
        $launcherTouched = $true
        $wscript = Join-Path $env:SystemRoot "System32\wscript.exe"
        $taskArguments = "//B //NoLogo `"$launcher`""
        $action = New-ScheduledTaskAction -Execute $wscript -Argument $taskArguments -WorkingDirectory $dataDir
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
    $installedPlugin = Join-Path $pluginDir "plugin"
    $statusline = Join-Path $installedPlugin "statusline.ps1"
    Write-Output "Claude plugin: $installedPlugin"
    if ($skipService) {
        Write-Output "Login startup skipped. Start manually:"
        Write-Output "  `"$binary`" --config `"$configFile`" --db `"$dbFile`""
    }
    if ($null -ne (Get-Command claude -ErrorAction SilentlyContinue)) {
        Write-Output "Claude Code detected. Native Windows hooks are installed."
    } else {
        Write-Output "Claude Code not found."
    }
    $settingsFile = Join-Path $HOME ".claude\appa-session-settings.json"
    $statuslinePath = $statusline.Replace('\', '/')
    $statuslineCommand = "`"$powershellPath`" -NoProfile -ExecutionPolicy Bypass -File `"$statuslinePath`""
    Write-Output "Create $settingsFile with:"
    Write-Output (@{
        statusLine = @{
            type = "command"
            command = $statuslineCommand
        }
    } | ConvertTo-Json -Depth 3)
    $profileSettings = $settingsFile.Replace("'", "''")
    $profilePlugin = $installedPlugin.Replace("'", "''")
    Write-Output "Add this function to your PowerShell profile:"
    Write-Output "  function clappa { claude --settings '$profileSettings' --plugin-dir '$profilePlugin' @args }"
    Write-Output "Only clappa sessions are gated. They block while the runtime task is down."
    Write-Output "Start clappa and run /appa-tool-sync to declare installed MCP tools."
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
        if ($launcherTouched) {
            if ($null -eq $oldLauncher) {
                Remove-Item -LiteralPath $launcher -Force -ErrorAction SilentlyContinue
            } else {
                Set-Content -LiteralPath $launcher -Value $oldLauncher -Encoding Unicode
            }
        }
        if ($taskTouched -and $null -ne $oldTaskXml) {
            Register-ScheduledTask -TaskName $script:TaskName -Xml $oldTaskXml -Force | Out-Null
            if ($oldTaskWasRunning) {
                Start-ScheduledTask -TaskName $script:TaskName
            }
        }
        if ($legacyTaskTouched -and $null -ne $legacyTaskXml) {
            Register-ScheduledTask -TaskName $script:LegacyTaskName -Xml $legacyTaskXml -Force | Out-Null
            if ($legacyTaskWasRunning) {
                Start-ScheduledTask -TaskName $script:LegacyTaskName
            }
        }
    } catch {
        Write-Warning "Installation failed and rollback was incomplete: $($_.Exception.Message)"
    }
    throw $installFailure
} finally {
    Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
}
