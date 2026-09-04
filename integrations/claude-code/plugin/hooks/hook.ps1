param(
    [switch]$SessionContext,
    [switch]$EnsureRuntime,
    # The hooks that report a finished turn pass this. They decide nothing, so
    # they never block, and they take the shorter deadline: a turn end waits on
    # no evidence round trip. The map declares it beside the event it registers
    # so nothing in a posted event can move a hook's blocking outcome.
    [switch]$TurnEnd
)

# Hooks protect only sessions launched with APPA_GATE=1 (the clappa
# function). The guard reads the Claude Code process environment, fixed at
# launch, so a session cannot turn the protection off. Without the variable
# the protection hook exits 0 and -SessionContext prints nothing.
# Installing the runtime is `appa init claude-code`'s job.
$protected = $env:APPA_GATE -eq "1"

# appa init claude-code renders these into the deployment: the absolute binary,
# config and data paths it resolved, and the endpoint every consumer shares.
# Nothing here consults PATH or APPA_INSTALL_DIR.
. (Join-Path $PSScriptRoot "appa-paths.ps1")
$binary = $AppaBin

if ($SessionContext) {
    if (-not $protected) {
        exit 0
    }
    Get-Content -LiteralPath (Join-Path $PSScriptRoot "session-context.md") -Raw
    exit 0
}

# APPA_RUNTIME_URL keeps both of its jobs: the URL a client talks to, and the
# signal that the runtime answering it is the user's own to restart.
$runtimeUrl = if ($env:APPA_RUNTIME_URL) {
    $env:APPA_RUNTIME_URL.TrimEnd("/")
} else {
    $AppaEndpoint
}

function Get-HealthAnswer {
    try {
        [string](Invoke-RestMethod -Uri "$runtimeUrl/health" -TimeoutSec 1 -UseBasicParsing)
    } catch {
        ""
    }
}

# A runtime the user runs at a URL of their own (APPA_RUNTIME_URL, the
# development setup) is theirs to restart: it is healthy while it
# answers, stale or not. The starter replaces only the default deployment.
function Test-RuntimeHealthy {
    $answer = Get-HealthAnswer
    if ($answer -eq "ok") {
        return $true
    }
    ($answer -like "stale *") -and [bool]$env:APPA_RUNTIME_URL
}

# A runtime answers `stale <pid>` once an install replaced its binary on
# disk: the process still serves the build it started from. Stopping it
# makes the install take effect, at the cost of the protected sessions
# already open, whose hooks fail closed until the start answers. The pid
# arrives in an HTTP body from whoever holds the port, so only a process
# named appa is ever stopped. Returns $true once the port refuses
# or another starter has already replaced the runtime, $false when the
# stale runtime cannot be stopped.
function Stop-StaleRuntime {
    $answer = Get-HealthAnswer
    if ($answer -notmatch '^stale ([1-9][0-9]*)$') {
        return $true
    }
    $stalePid = [int]$Matches[1]
    # No process at that pid: a concurrent starter already stopped it, and
    # the wait below sees the port refuse or that starter's replacement.
    $process = Get-Process -Id $stalePid -ErrorAction SilentlyContinue
    if ($null -ne $process) {
        $expectedPath = if ($process.ProcessName -eq "appa") { $binary } else { $null }
        if ($null -eq $expectedPath -or -not [String]::Equals(
            $process.Path,
            $expectedPath,
            [StringComparison]::OrdinalIgnoreCase
        )) {
            [Console]::Error.WriteLine("appa protection: pid $stalePid is not appa runtime; not stopping it")
            return $false
        }
        try {
            Stop-Process -Id $stalePid -ErrorAction Stop
        } catch {
            # Gone between the lookup and the stop: the same concurrent-starter
            # race as no process at all, settled by the wait below.
            if ($null -ne (Get-Process -Id $stalePid -ErrorAction SilentlyContinue)) {
                [Console]::Error.WriteLine("appa protection: cannot stop the stale runtime (pid $stalePid): $($_.Exception.Message)")
                return $false
            }
        }
    }
    $deadline = (Get-Date).AddSeconds(10)
    while ((Get-Date) -lt $deadline) {
        $now = Get-HealthAnswer
        if ($now -eq "" -or $now -eq "ok") {
            return $true
        }
        if ($now -ne $answer) {
            [Console]::Error.WriteLine("appa protection: $runtimeUrl answers neither ok nor the stale runtime being stopped")
            return $false
        }
        Start-Sleep -Seconds 1
    }
    [Console]::Error.WriteLine("appa protection: the stale runtime at $runtimeUrl (pid $stalePid) did not stop")
    $false
}

# SessionStart starts the installed runtime when nothing healthy answers,
# so a protected session works without any service setup, and the last step
# of the install starts it through -EnsureRuntime. A runtime whose binary
# an install replaced is stopped first. Installing the binary is
# not this script's job: `appa init claude-code` does that first.
#
# Returns $true only while a healthy runtime answers, and $false on every way
# of failing to get one -- a stale runtime that would not stop, a missing
# binary, a start that never became healthy. The caller must block on $false:
# posting anyway would send the event to the stale runtime this install just
# replaced, which is the skew the whole bundle exists to prevent.
function Start-RuntimeIfDown {
    if (Test-RuntimeHealthy) {
        return $true
    }
    if (-not (Stop-StaleRuntime)) {
        return $false
    }
    if (Test-RuntimeHealthy) {
        return $true
    }
    if (-not (Test-Path -LiteralPath $binary)) {
        [Console]::Error.WriteLine("appa protection: appa is not installed; expected at $binary")
        return $false
    }
    # The runtime writes the default policy on its first start and refuses
    # to start when it cannot. The policy and the database live in two
    # different directories on Windows, so both must exist first.
    New-Item -ItemType Directory -Path (Split-Path -Parent $AppaConfig) -Force | Out-Null
    New-Item -ItemType Directory -Path $AppaDataDir -Force | Out-Null
    # Start-Process joins ArgumentList into one Windows command line. Quote the
    # path tokens explicitly so the standard user directories may contain spaces.
    $databasePath = Join-Path $AppaDataDir "appa.db"
    Start-Process -FilePath $binary -WindowStyle Hidden -ArgumentList @(
        "runtime",
        "--listen", $AppaListen,
        "--config", "`"$AppaConfig`"",
        "--db", "`"$databasePath`""
    ) | Out-Null
    # A wall-clock budget, not a count of probes: one probe is instant
    # where the port refuses and costs the full deadline where it hangs.
    # The whole start must fit inside the timeout hooks.windows.json
    # declares for SessionStart.
    $deadline = (Get-Date).AddSeconds(20)
    while ((Get-Date) -lt $deadline) {
        if (Test-RuntimeHealthy) {
            return $true
        }
        Start-Sleep -Seconds 1
    }
    $false
}

# The last step of the install starts the runtime through this switch, so
# the install and every protected session start share one starter. It runs
# outside the protection guard, because the session that installs is not
# protected. It exits 0 only while /health answers, which is the proof the
# install reports.
if ($EnsureRuntime) {
    if (-not (Test-Path -LiteralPath $binary)) {
        [Console]::Error.WriteLine("appa protection: appa is not installed; expected at $binary")
        exit 1
    }
    if (Start-RuntimeIfDown) {
        exit 0
    }
    [Console]::Error.WriteLine("appa protection: runtime did not become healthy at $runtimeUrl. Its own error is the last line of $(Join-Path $AppaDataDir 'runtime.stderr.log')")
    exit 1
}

if (-not $protected) {
    exit 0
}

# The subagent definitions in reach that declare maxTurns. Claude Code ends
# such a subagent at its turn cap with no SubagentStop, so the return check
# never runs and the parent receives its partial output unchecked; a prompt
# is refused while one exists. The project and user agent directories and
# the installed plugins' agent directories are scanned; agents passed on the
# command line (--agents, --plugin-dir) are not.
function Find-MaxTurnsAgents {
    $projectDir = if ($env:CLAUDE_PROJECT_DIR) { $env:CLAUDE_PROJECT_DIR } else { (Get-Location).Path }
    $found = @()
    foreach ($dir in @((Join-Path $projectDir ".claude\agents"), (Join-Path $HOME ".claude\agents"))) {
        if (-not (Test-Path -LiteralPath $dir)) {
            continue
        }
        foreach ($file in Get-ChildItem -LiteralPath $dir -Filter *.md -File) {
            if (Select-String -LiteralPath $file.FullName -Pattern '^maxTurns:' -Quiet) {
                $found += $file.FullName
            }
        }
    }
    $plugins = Join-Path $HOME ".claude\plugins\cache"
    if (Test-Path -LiteralPath $plugins) {
        foreach ($file in Get-ChildItem -LiteralPath $plugins -Recurse -Filter *.md -File) {
            if ((Split-Path -Leaf (Split-Path -Parent $file.FullName)) -eq "agents" -and
                (Select-String -LiteralPath $file.FullName -Pattern '^maxTurns:' -Quiet)) {
                $found += $file.FullName
            }
        }
    }
    ,$found
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
        # The POSIX map chains `ensure-runtime.sh && hook.sh || exit 2`, so a
        # starter that fails there never reaches the post. This is that chain:
        # throwing hands the failure to the same catch every other unanswered
        # hook takes, which blocks. Posting anyway would send the event to the
        # stale runtime the install just replaced.
        if (-not (Start-RuntimeIfDown)) {
            throw "the runtime at $runtimeUrl is not healthy and could not be started"
        }
    }
    if ($null -ne $hookInput -and $hookInput.hook_event_name -eq "UserPromptSubmit") {
        $declaring = Find-MaxTurnsAgents
        if ($declaring.Count -gt 0) {
            throw "[appa] this session cannot be protected while a subagent definition declares maxTurns: Claude Code ends that subagent without the return check and hands the parent its partial output unchecked. Remove maxTurns from: $($declaring -join ', ')"
        }
    }
    # The installed binary is the hook client: it translates the Claude Code event
    # onto the runtime's wire, posts it, renders the answer back into Claude Code's
    # shape, and exits 2 on a refusal or no answer (0 on a turn end, which decides
    # nothing). Its stdout and exit code are this hook's.
    $hookArgs = @("hook", "--adapter", "claude-code", "--url", $runtimeUrl)
    if ($TurnEnd) {
        $hookArgs += "--turn-end"
    }
    $answer = $payload | & $binary @hookArgs
    # A native command that exits non-zero throws nothing, so the catch below
    # never sees a client that failed: the code is read here, before anything
    # else can move it. A failure that rendered nothing is handed to that catch,
    # because on a PostToolUse the exit code alone leaves the tool's own output
    # in front of the model, which is the fail-open this hook exists to prevent.
    # A failure that did render keeps its answer: that is the runtime's own
    # refusal, whose replacement is shaped for the result it withholds.
    $clientExit = $LASTEXITCODE
    $rendered = ($answer -join "`n")
    [Console]::Out.Write($rendered)
    if ($clientExit -ne 0 -and [string]::IsNullOrEmpty($rendered)) {
        throw "the hook client exited $clientExit without an answer"
    }
    exit $clientExit
} catch {
    $failure = $_.Exception.Message
    # A turn end decides nothing, and every blocking outcome on these
    # hooks means "do not stop", which would hold the actor in a turn it
    # has finished. A runtime that does not answer costs a call left
    # open, which the next turn end closes.
    if ($TurnEnd) {
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
