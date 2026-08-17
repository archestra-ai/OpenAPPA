$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$installer = Join-Path $repositoryRoot "install.ps1"
$tempDir = Join-Path ([IO.Path]::GetTempPath()) "appa-installer-test-$([Guid]::NewGuid())"
$releaseDir = Join-Path $tempDir "release"
$packageDir = Join-Path $tempDir "package"
$server = $null

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
public static class Program {
    public static int Main(string[] args) {
        if (args.Length == 1 && args[0] == "--version") {
            Console.WriteLine("appa-runtime-v2 $version");
        }
        return 0;
    }
}
"@
    Add-Type -TypeDefinition $source -Language CSharp -OutputAssembly $sourceBinary -OutputType ConsoleApplication
    "{}" | Set-Content -LiteralPath (Join-Path $packageDir "claude-code\.claude-plugin\marketplace.json")
    "{}" | Set-Content -LiteralPath (Join-Path $packageDir "claude-code\plugin\.claude-plugin\plugin.json")
    "{}" | Set-Content -LiteralPath (Join-Path $packageDir "claude-code\plugin\.mcp.json")
    "{}" | Set-Content -LiteralPath (Join-Path $packageDir "claude-code\plugin\hooks\hooks.json")
    "session context" | Set-Content -LiteralPath (Join-Path $packageDir "claude-code\plugin\hooks\session-context.md")
    "tool sync" | Set-Content -LiteralPath (Join-Path $packageDir "claude-code\plugin\skills\appa-tool-sync\SKILL.md")
    "#!/bin/sh`nexit 0" | Set-Content -LiteralPath (Join-Path $packageDir "claude-code\plugin\statusline.sh")

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

    $env:APPA_INSTALL_DIR = Join-Path $tempDir "install\bin"
    $env:APPA_CONFIG_DIR = Join-Path $tempDir "install\config"
    $env:APPA_DATA_DIR = Join-Path $tempDir "install\data"
    $env:APPA_DOWNLOAD_BASE = "http://127.0.0.1:$port"
    $env:APPA_SKIP_SERVICE = "1"

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

    $output = (& $installer | Out-String)
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
    if (-not (Test-Path -LiteralPath (Join-Path $installedPlugin "plugin\hooks\session-context.md"))) {
        throw "Session context was not installed"
    }
    if (-not (Test-Path -LiteralPath (Join-Path $installedPlugin "plugin\skills\appa-tool-sync\SKILL.md"))) {
        throw "Tool-sync skill was not installed"
    }
    if ((& $installedBinary --version | Out-String).Trim() -ne "appa-runtime-v2 $version") {
        throw "Installed version is wrong"
    }

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

    & $installer -Uninstall | Out-Null
    if (Test-Path -LiteralPath $installedBinary) { throw "Runtime survived uninstall" }
    if (Test-Path -LiteralPath $installedPlugin) { throw "Plugin survived uninstall" }
    if ((Get-Content -LiteralPath (Join-Path $env:APPA_CONFIG_DIR "appa.toml") -Raw).Trim() -ne "policy survives update") {
        throw "Uninstall removed policy"
    }
    if ((Get-Content -LiteralPath (Join-Path $env:APPA_DATA_DIR "appa.db") -Raw).Trim() -ne "database survives update") {
        throw "Uninstall removed database"
    }

    Write-Output "Windows installer tests passed."
} finally {
    if ($null -ne $server -and -not $server.HasExited) {
        Stop-Process -Id $server.Id -Force
    }
    Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
}
