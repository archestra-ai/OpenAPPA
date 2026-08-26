---
name: appa-setup
description: Install or upgrade the OpenAPPA runtime for this Claude Code plugin - download the appa-runtime release binary, verify its checksum and version, install the clappa command and the APPA statusline, start the runtime, and ask whether the install may be counted. Use when the user asks to set up APPA, install or upgrade appa-runtime, or when a session reports that the runtime binary is not installed.
---

# appa-setup

Install the `appa-runtime` binary that this plugin's hooks protect
sessions with, plus the `clappa` command and the statusline, then start
the runtime and prove it answers. Every command runs under the
session's normal command approval.

## Resolve two paths first

The install must agree with the plugin's hooks on where things live.
Resolve both paths in a shell; do not guess.

- **Install target** — where the hooks look for the binary. On POSIX
  systems: `${APPA_INSTALL_DIR:-$HOME/.local/bin}/appa-runtime`. On
  native Windows: `appa-runtime.exe` in `$env:APPA_INSTALL_DIR`, or
  `<data dir>\bin` when that is unset, where the data dir is
  `$env:APPA_DATA_DIR` or `$env:LOCALAPPDATA\appa`.
- **Plugin files** — the directory holding the plugin's `hooks/` and
  statusline scripts. This skill lives at
  `<plugin files>/skills/appa-setup`, so the plugin files directory is
  two levels up from this skill's base directory. When the base
  directory is not known, find a marketplace install with
  `ls -d ~/.claude/plugins/cache/*/appa-runtime/*/` and take the newest
  version directory, or ask the user where the plugin checkout is.

## Install the runtime

1. Pick the release archive for this system: `appa-runtime-<arch>-<os>.tar.gz`, where `<arch>` is `aarch64` or `x86_64` (`uname -m`) and `<os>` is `apple-darwin` or `unknown-linux-gnu` (`uname -s`). On native Windows it is `appa-runtime-<arch>-pc-windows-msvc.zip`.
2. Download the archive plus `SHA256SUMS` and `version.txt` into a temporary directory: `curl -fsSL -O https://github.com/archestra-ai/OpenAPPA/releases/latest/download/<asset>` for each of the three assets. No authentication is needed. `gh release download --repo archestra-ai/OpenAPPA --pattern <asset>` works too when `gh` is already installed.
3. Verify before anything runs: the archive's SHA-256 must equal its line in `SHA256SUMS` (`shasum -a 256` or `sha256sum`). On a mismatch, stop and tell the user; do not install.
4. Extract the archive and check the binary: `./appa-runtime --version` must print exactly `appa-runtime <contents of version.txt>`.
5. Install the binary to the install target resolved above, mode 755, creating the directory when needed. That exact path is where the plugin's hooks look for it; do not choose a different location. Step 8 starts it.
6. Create the `clappa` command as an executable, not an alias, so it works in every open terminal with no shell reload: write `clappa` into the same directory as the runtime binary, mode 755, containing:

   ```sh
   #!/bin/sh
   exec env APPA_GATE=1 claude "$@"
   ```

   Only if that directory is not on the user's `PATH`, fall back to appending `alias clappa='APPA_GATE=1 claude'` to the file matching their shell and tell them to reload it. On Windows, add the matching `clappa` function to the PowerShell profile.

7. Install the statusline, unless `~/.claude/settings.json` already has a `statusLine` entry that runs something other than `appa-statusline.sh` — never replace someone else's. Overwrite an APPA entry that names a different path: an earlier install into another directory leaves one behind. Copy `statusline.sh` (on Windows, `statusline.ps1`) from the plugin files directory resolved above into the runtime binary's directory as `appa-statusline.sh`, mode 755, and merge into `~/.claude/settings.json`:

   ```json
   {"statusLine": {"type": "command", "command": "<that path>/appa-statusline.sh"}}
   ```

   It shows the APPA mascot with the session's trust and audience when protected, and the mascot alone when not.

8. Start the runtime and confirm it answers. First ask whether one already runs: `curl -sS -m 2 http://127.0.0.1:8787/health`, or `$APPA_RUNTIME_URL/health` when that variable is set.

   - Nothing answers — the usual case on a first install. Start it with the plugin's own starter, `sh "<plugin files>/hooks/ensure-runtime.sh"`, taking `<plugin files>` from the path resolved above. On native Windows, run `powershell.exe -NoProfile -File "<plugin files>\hooks\hook.ps1" -EnsureRuntime`. The starter runs the installed binary and exits 0 only after `/health` answers `ok`; every protected session start runs the same script. Then repeat the `/health` request yourself and report what it printed. The first start also writes the default policy, so step 10's `/appa-guide init` tip has a file to work on.
   - The request prints `stale <pid>` — a runtime from an earlier install is still running and has seen the binary you just installed replace its own. Warn the user that replacing it blocks every protected session that is already open until the new runtime answers, because the hooks fail closed, then run the same starter: it stops that process, starts the installed build, and exits 0 only after `/health` answers `ok`. Repeat the `/health` request yourself and report what it printed.
   - The request prints `ok` — the running runtime already serves the installed binary. Report it as running; there is nothing to start.

   If the starter exits non-zero, the runtime is not running. Report that, quote the last lines of `runtime.stderr.log` in the data directory, and stop. Do not describe the setup as finished.

9. Ask whether to report the install, and send nothing unless the answer is
   yes. Ask in one short question, in your own words, covering exactly this:
   the project counts installs, one event says which version and which OS and
   architecture, nothing that identifies them or their machine is included or
   stored, and `APPA_TELEMETRY=0` refuses it permanently.

   Treat only a clear yes as yes. Silence, a shrug, "whatever", or moving on to
   another question are all no. Do not ask twice, do not argue with a no, and
   do not report it later in the session. A no costs the user nothing else: the
   install is already finished either way.

   On a yes, run the reporter once, taking `<version>` from the `version.txt`
   downloaded in step 2 and `<os>` and `<arch>` from the asset you picked in
   step 1:

   ```sh
   sh "<plugin files>/report-install.sh" <version> <os> <arch>
   ```

   On native Windows:

   ```powershell
   powershell.exe -NoProfile -File "<plugin files>\report-install.ps1" -Version <version> -Os <os> -Arch <arch>
   ```

   It exits 0 whether or not the event reaches anyone. Do not check it, retry
   it, or mention it again — a failed count is not the user's problem, and an
   install that worked must not be reported as broken because a metric missed.

10. Finish by telling the user that the runtime is running and that `clappa` starts a protected session. Add this tip on the next line: "🚀 Run `/appa-guide init` in the `clappa` session to build your initial security policy." Keep the command and `clappa` as inline code.

If the `curl` download fails, ask the user to install the GitHub CLI, then try again with `gh release download`.
