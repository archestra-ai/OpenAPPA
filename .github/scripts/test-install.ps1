$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$installer = Join-Path $repositoryRoot "install.ps1"
$tempDir = Join-Path ([IO.Path]::GetTempPath()) "appa-installer-test-$([Guid]::NewGuid())"
$releaseDir = Join-Path $tempDir "release"
$packageDir = Join-Path $tempDir "package"
$server = $null
$taskName = "appa-runtime-v2-$([Security.Principal.WindowsIdentity]::GetCurrent().User.Value)"

try {
    New-Item -ItemType Directory -Path $releaseDir | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $packageDir "claude-code\.claude-plugin") | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $packageDir "claude-code\plugin\.claude-plugin") | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $packageDir "claude-code\plugin\hooks") | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $packageDir "claude-code\plugin\skills\appa-tool-sync") | Out-Null

    $version = "9.8.7"
    $architecture = if ($env:PROCESSOR_ARCHITEW6432) {
        $env:PROCESSOR_ARCHITEW6432
    } else {
        $env:PROCESSOR_ARCHITECTURE
    }
    switch ($architecture.ToUpperInvariant()) {
        "AMD64" { $target = "x86_64-pc-windows-msvc" }
        "ARM64" { $target = "aarch64-pc-windows-msvc" }
        default { throw "Unsupported test architecture: $architecture" }
    }
    $archive = "appa-runtime-v2-$target.zip"
    $sourceBinary = Join-Path $packageDir "appa-runtime-v2.exe"
    $source = @"
using System;
using System.IO;
using System.Net;
using System.Net.Sockets;
using System.Text;
public static class Program {
    public static int Main(string[] args) {
        if (Array.IndexOf(args, "--version") >= 0) {
            Console.WriteLine("appa-runtime-v2 $version");
            return 0;
        }
        var instanceId = "";
        for (var index = 0; index + 1 < args.Length; index++) {
            if (args[index] == "--instance-id") {
                instanceId = args[index + 1];
            }
        }
        var listener = new TcpListener(IPAddress.Loopback, 8787);
        listener.Start();
        while (true) {
            using (var client = listener.AcceptTcpClient())
            using (var stream = client.GetStream())
            using (var reader = new StreamReader(stream, Encoding.ASCII, false, 1024, true)) {
                var requestLine = reader.ReadLine() ?? "";
                string header;
                do { header = reader.ReadLine(); } while (!String.IsNullOrEmpty(header));
                var path = requestLine.Split(' ')[1];
                var body = path.StartsWith("/status")
                    ? "{\"trust\":\"high\",\"audience\":\"private\"}"
                    : path == "/health" ? "ok" : "{}";
                var bytes = Encoding.UTF8.GetBytes(body);
                var response = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n" +
                    "Content-Length: " + bytes.Length + "\r\n" +
                    "X-Appa-Instance-Id: " + instanceId + "\r\nConnection: close\r\n\r\n";
                var responseBytes = Encoding.ASCII.GetBytes(response);
                stream.Write(responseBytes, 0, responseBytes.Length);
                stream.Write(bytes, 0, bytes.Length);
            }
        }
    }
}
"@
    Add-Type -TypeDefinition $source -Language CSharp -OutputAssembly $sourceBinary -OutputType ConsoleApplication
    "{}" | Set-Content -LiteralPath (Join-Path $packageDir "claude-code\.claude-plugin\marketplace.json")
    "{}" | Set-Content -LiteralPath (Join-Path $packageDir "claude-code\plugin\.claude-plugin\plugin.json")
    "{}" | Set-Content -LiteralPath (Join-Path $packageDir "claude-code\plugin\.mcp.json")
    '{"platform":"posix"}' | Set-Content -LiteralPath (Join-Path $packageDir "claude-code\plugin\hooks\hooks.json")
    Copy-Item -LiteralPath (Join-Path $repositoryRoot "integrations\claude-code\plugin\hooks\hooks.windows.json") `
        -Destination (Join-Path $packageDir "claude-code\plugin\hooks\hooks.windows.json")
    Copy-Item -LiteralPath (Join-Path $repositoryRoot "integrations\claude-code\plugin\hooks\hook.ps1") `
        -Destination (Join-Path $packageDir "claude-code\plugin\hooks\hook.ps1")
    "session context" | Set-Content -LiteralPath (Join-Path $packageDir "claude-code\plugin\hooks\session-context.md")
    "tool sync" | Set-Content -LiteralPath (Join-Path $packageDir "claude-code\plugin\skills\appa-tool-sync\SKILL.md")
    "#!/bin/sh`nexit 0" | Set-Content -LiteralPath (Join-Path $packageDir "claude-code\plugin\statusline.sh")
    Copy-Item -LiteralPath (Join-Path $repositoryRoot "integrations\claude-code\plugin\statusline.ps1") `
        -Destination (Join-Path $packageDir "claude-code\plugin\statusline.ps1")

    $archivePath = Join-Path $releaseDir $archive
    Compress-Archive -Path (Join-Path $packageDir "*") -DestinationPath $archivePath
    $version | Set-Content -LiteralPath (Join-Path $releaseDir "version.txt")
    $checksum = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    "$checksum  $archive" | Set-Content -LiteralPath (Join-Path $releaseDir "SHA256SUMS")

    $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    $listener.Start()
    $port = ([Net.IPEndPoint]$listener.LocalEndpoint).Port
    $listener.Stop()
    $server = Start-Process python -ArgumentList @("-m", "http.server", "$port", "--bind", "127.0.0.1") `
        -WorkingDirectory $releaseDir -PassThru -WindowStyle Hidden
    for ($attempt = 0; $attempt -lt 30; $attempt++) {
        try {
            Invoke-WebRequest -Uri "http://127.0.0.1:$port/version.txt" -UseBasicParsing | Out-Null
            break
        } catch {
            if ($attempt -eq 29) { throw "Fixture HTTP server did not start" }
            Start-Sleep -Milliseconds 100
        }
    }

    $env:APPA_INSTALL_DIR = Join-Path $tempDir "install files\bin"
    $env:APPA_CONFIG_DIR = Join-Path $tempDir "install files\config"
    $env:APPA_DATA_DIR = Join-Path $tempDir "install files\data"
    $env:APPA_DOWNLOAD_BASE = "http://127.0.0.1:$port"
    $env:APPA_SKIP_SERVICE = "0"

    $validInstallDir = $env:APPA_INSTALL_DIR
    $env:APPA_INSTALL_DIR = "C:relative"
    try {
        & $installer | Out-Null
        throw "Relative installation directory was accepted"
    } catch {
        if ($_.Exception.Message -notlike "Installation directories must be absolute:*") { throw }
    } finally {
        $env:APPA_INSTALL_DIR = $validInstallDir
    }

    $validDownloadBase = $env:APPA_DOWNLOAD_BASE
    $env:APPA_DOWNLOAD_BASE = "http://127.0.0.1:80@example.com"
    try {
        & $installer | Out-Null
        throw "Non-loopback plaintext URL was accepted"
    } catch {
        if ($_.Exception.Message -notlike "APPA_DOWNLOAD_BASE must use HTTPS, file, or loopback HTTP*") { throw }
    } finally {
        $env:APPA_DOWNLOAD_BASE = $validDownloadBase
    }

    $currentUser = [Security.Principal.WindowsIdentity]::GetCurrent().Name
    $legacyAction = New-ScheduledTaskAction -Execute $env:ComSpec -Argument "/c exit 0"
    $legacyTrigger = New-ScheduledTaskTrigger -AtLogOn -User $currentUser
    $legacyPrincipal = New-ScheduledTaskPrincipal -UserId $currentUser -LogonType Interactive -RunLevel Limited
    Register-ScheduledTask -TaskName "appa-runtime-v2" -Action $legacyAction -Trigger $legacyTrigger `
        -Principal $legacyPrincipal -Force | Out-Null

    $escapedInstaller = $installer.Replace("'", "''")
    $output = (& powershell.exe -NoProfile -Command `
        "Get-Content -LiteralPath '$escapedInstaller' -Raw | Invoke-Expression" | Out-String)
    if ($LASTEXITCODE -ne 0) {
        throw "Streamed installer invocation failed"
    }
    if (-not $output.Contains("Installed appa-runtime-v2 $version.")) {
        throw "Installer did not report installed version"
    }
    $installedBinary = Join-Path $env:APPA_INSTALL_DIR "appa-runtime-v2.exe"
    $installedPlugin = Join-Path $env:APPA_DATA_DIR "claude-code"
    if (-not (Test-Path -LiteralPath $installedBinary -PathType Leaf)) { throw "Runtime was not installed" }
    if (-not (Test-Path -LiteralPath (Join-Path $installedPlugin ".claude-plugin\marketplace.json"))) {
        throw "Marketplace was not installed"
    }
    if (-not (Test-Path -LiteralPath (Join-Path $installedPlugin "plugin\.claude-plugin\plugin.json"))) {
        throw "Plugin manifest was not installed"
    }
    if (-not (Test-Path -LiteralPath (Join-Path $installedPlugin "plugin\.mcp.json"))) {
        throw "MCP configuration was not installed"
    }
    if (-not (Test-Path -LiteralPath (Join-Path $installedPlugin "plugin\hooks\hooks.json"))) {
        throw "Plugin was not installed"
    }
    $installedHookConfig = Get-Content -LiteralPath `
        (Join-Path $installedPlugin "plugin\hooks\hooks.json") -Raw | ConvertFrom-Json
    $hookConfigBytes = [IO.File]::ReadAllBytes((Join-Path $installedPlugin "plugin\hooks\hooks.json"))
    if ($hookConfigBytes.Length -ge 3 -and $hookConfigBytes[0] -eq 0xEF -and
        $hookConfigBytes[1] -eq 0xBB -and $hookConfigBytes[2] -eq 0xBF) {
        throw "Installed hook configuration has a UTF-8 BOM"
    }
    $hookCommand = $installedHookConfig.hooks.PreToolUse[0].hooks[0].command
    if (-not [IO.Path]::IsPathRooted($hookCommand) -or
        [IO.Path]::GetFileName($hookCommand) -notmatch '^(powershell|pwsh)\.exe$') {
        throw "Windows hook configuration was not selected"
    }
    if (-not (Test-Path -LiteralPath (Join-Path $installedPlugin "plugin\hooks\hook.ps1"))) {
        throw "Windows hook adapter was not installed"
    }
    if (-not (Test-Path -LiteralPath (Join-Path $installedPlugin "plugin\statusline.ps1"))) {
        throw "Windows statusline was not installed"
    }
    if (-not (Test-Path -LiteralPath (Join-Path $installedPlugin "plugin\hooks\session-context.md"))) {
        throw "Session context was not installed"
    }
    if (-not (Test-Path -LiteralPath (Join-Path $installedPlugin "plugin\skills\appa-tool-sync\SKILL.md"))) {
        throw "Tool-sync skill was not installed"
    }
    if ((& $installedBinary --version | Out-String).Trim() -ne "appa-runtime-v2 $version") {
        throw "Installed version is wrong"
    }
    $installedTask = Get-ScheduledTask -TaskName $taskName
    if ($null -ne (Get-ScheduledTask -TaskName "appa-runtime-v2" -ErrorAction SilentlyContinue)) {
        throw "Legacy machine-global Scheduled Task was not migrated"
    }
    $taskAction = $installedTask.Actions[0]
    if ($taskAction.Execute -notmatch 'wscript\.exe$' -or
        $taskAction.Arguments -notlike '*appa-runtime-v2.vbs*') {
        throw "Scheduled Task does not run the runtime through the windowless launcher"
    }
    if ($installedTask.Triggers[0].CimClass.CimClassName -ne "MSFT_TaskLogonTrigger") {
        throw "Scheduled Task does not start at user login"
    }
    if ($installedTask.Principal.UserId -ne [Security.Principal.WindowsIdentity]::GetCurrent().Name) {
        throw "Scheduled Task is not scoped to the current user principal"
    }
    $launcher = Join-Path $env:APPA_DATA_DIR "appa-runtime-v2.vbs"
    $launcherContent = Get-Content -LiteralPath $launcher -Raw
    if ($launcherContent -notmatch 'startupConfig\.ShowWindow = 0' -or
        $launcherContent -notmatch 'processConfig\.Create') {
        throw "Runtime launcher does not create a hidden process"
    }
    $pidFile = Join-Path $env:APPA_DATA_DIR "appa-runtime-v2.pid"
    if (-not (Test-Path -LiteralPath $pidFile -PathType Leaf)) {
        throw "Runtime launcher did not record the child process"
    }
    $env:APPA_RUNTIME_URL = "http://127.0.0.1:8787"
    $hookScript = Join-Path $installedPlugin "plugin\hooks\hook.ps1"
    $hookOutput = ('{"hook_event_name":"PreToolUse"}' |
        powershell.exe -NoProfile -ExecutionPolicy Bypass -File $hookScript | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $hookOutput -ne "{}") {
        throw "Windows hook adapter did not relay the runtime response"
    }
    '{"hook_event_name":"PreToolUse"}' |
        powershell.exe -NoProfile -ExecutionPolicy Bypass -File $hookScript -SessionContext 2>$null | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "Windows session context hook failed"
    }
    $statuslineScript = Join-Path $installedPlugin "plugin\statusline.ps1"
    $statuslineOutput = ('{"session_id":"fixture"}' |
        powershell.exe -NoProfile -ExecutionPolicy Bypass -File $statuslineScript | Out-String)
    if (-not $statuslineOutput.Contains("trust:high  audience:private")) {
        throw "Windows statusline did not render runtime status"
    }
    $env:APPA_RUNTIME_URL = "http://127.0.0.1:1"
    '{"hook_event_name":"PreToolUse"}' |
        powershell.exe -NoProfile -ExecutionPolicy Bypass -File $hookScript 2>$null | Out-Null
    if ($LASTEXITCODE -ne 2) {
        throw "Windows hook adapter did not fail closed while the runtime was down"
    }
    $postToolOutput = ('{"hook_event_name":"PostToolUse"}' |
        powershell.exe -NoProfile -ExecutionPolicy Bypass -File $hookScript 2>$null | Out-String).Trim() |
        ConvertFrom-Json
    if ($LASTEXITCODE -ne 0 -or $postToolOutput.decision -ne "block" -or
        -not $postToolOutput.hookSpecificOutput.updatedToolOutput) {
        throw "Windows hook adapter did not block a tool result while the runtime was down"
    }
    $env:APPA_RUNTIME_URL = "http://127.0.0.1:8787"

    "policy survives update" | Set-Content -LiteralPath (Join-Path $env:APPA_CONFIG_DIR "appa.toml")
    "database survives update" | Set-Content -LiteralPath (Join-Path $env:APPA_DATA_DIR "appa.db")
    & $installer | Out-Null
    if ((Get-Content -LiteralPath (Join-Path $env:APPA_CONFIG_DIR "appa.toml") -Raw).Trim() -ne "policy survives update") {
        throw "Policy was replaced"
    }
    if ((Get-Content -LiteralPath (Join-Path $env:APPA_DATA_DIR "appa.db") -Raw).Trim() -ne "database survives update") {
        throw "Database was replaced"
    }

    Add-Content -LiteralPath $archivePath -Value "tampered"
    $rejected = $false
    try {
        & $installer | Out-Null
    } catch {
        if ($_.Exception.Message -like "Checksum mismatch for *") {
            $rejected = $true
        } else {
            throw
        }
    }
    if (-not $rejected) { throw "Tampered archive was accepted" }
    if ((& $installedBinary --version | Out-String).Trim() -ne "appa-runtime-v2 $version") {
        throw "Failed update changed installed runtime"
    }

    & ([scriptblock]::Create((Get-Content -LiteralPath $installer -Raw))) -Uninstall | Out-Null
    if (Test-Path -LiteralPath $installedBinary) { throw "Runtime survived uninstall" }
    if (Test-Path -LiteralPath $installedPlugin) { throw "Plugin survived uninstall" }
    if (Test-Path -LiteralPath $launcher) { throw "Runtime launcher survived uninstall" }
    if (Test-Path -LiteralPath $pidFile) { throw "Runtime PID file survived uninstall" }
    if ((Get-Content -LiteralPath (Join-Path $env:APPA_CONFIG_DIR "appa.toml") -Raw).Trim() -ne "policy survives update") {
        throw "Uninstall removed policy"
    }
    if ((Get-Content -LiteralPath (Join-Path $env:APPA_DATA_DIR "appa.db") -Raw).Trim() -ne "database survives update") {
        throw "Uninstall removed database"
    }

    Write-Output "Windows installer tests passed."
} finally {
    foreach ($cleanupTask in @($taskName, "appa-runtime-v2")) {
        if ($null -ne (Get-ScheduledTask -TaskName $cleanupTask -ErrorAction SilentlyContinue)) {
            Stop-ScheduledTask -TaskName $cleanupTask -ErrorAction SilentlyContinue
            Unregister-ScheduledTask -TaskName $cleanupTask -Confirm:$false -ErrorAction SilentlyContinue
        }
    }
    if ($null -ne $server -and -not $server.HasExited) {
        Stop-Process -Id $server.Id -Force
    }
    Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
}
