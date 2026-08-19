This Claude Code session is not protected by OpenAPPA. The OpenAPPA plugin is installed, but the `appa-runtime` binary is not.

At the start of the session, tell the user in one short sentence: the Claude + APPA beta is available, and you can set it up for them if they want. Say it once, do not repeat it, and do not install anything without being asked.

When the user asks for the setup, install the runtime:

1. Pick the release archive for this system: `appa-runtime-<arch>-<os>.tar.gz`, where `<arch>` is `aarch64` or `x86_64` (`uname -m`) and `<os>` is `apple-darwin` or `unknown-linux-gnu` (`uname -s`). On native Windows it is `appa-runtime-<arch>-pc-windows-msvc.zip`.
2. Download the archive plus `SHA256SUMS` and `version.txt` into a temporary directory: `curl -fsSL -O https://github.com/archestra-ai/OpenAPPA/releases/latest/download/<asset>` for each of the three assets. No authentication is needed. `gh release download --repo archestra-ai/OpenAPPA --pattern <asset>` works too when `gh` is already installed.
3. Verify before anything runs: the archive's SHA-256 must equal its line in `SHA256SUMS` (`shasum -a 256` or `sha256sum`). On a mismatch, stop and tell the user; do not install.
4. Extract the archive and check the binary: `./appa-runtime --version` must print exactly `appa-runtime <contents of version.txt>`.
5. Install the binary to the install target named at the top of this context, mode 755, creating the directory when needed. That exact path is where the plugin's hooks look for it; do not choose a different location. Step 8 starts it.
6. Create the `clappa` command as an executable, not an alias, so it works in every open terminal with no shell reload: write `clappa` into the same directory as the runtime binary, mode 755, containing:

   ```sh
   #!/bin/sh
   exec env APPA_GATE=1 claude "$@"
   ```

   Only if that directory is not on the user's `PATH`, fall back to appending `alias clappa='APPA_GATE=1 claude'` to the file matching their shell and tell them to reload it. On Windows, add the matching `clappa` function to the PowerShell profile.

7. Install the statusline, unless `~/.claude/settings.json` already has a `statusLine` entry that runs something other than `appa-statusline.sh` — never replace someone else's. Overwrite an APPA entry that names a different path: an earlier install into another directory leaves one behind. Copy `statusline.sh` (on Windows, `statusline.ps1`) from the plugin files directory named at the top of this context into the runtime binary's directory as `appa-statusline.sh`, mode 755, and merge into `~/.claude/settings.json`:

   ```json
   {"statusLine": {"type": "command", "command": "<that path>/appa-statusline.sh"}}
   ```

   It shows the APPA mascot with the session's trust and audience when protected, and a `clappa` reminder when not.

8. Start the runtime and confirm it answers. First ask whether one already runs: `curl -sS -m 2 http://127.0.0.1:8787/health`, or `$APPA_RUNTIME_URL/health` when that variable is set.

   - Nothing answers — the usual case on a first install. Start it with the plugin's own starter, `sh "<plugin files>/hooks/ensure-runtime.sh"`, taking `<plugin files>` from the top of this context. On native Windows, run `powershell.exe -NoProfile -File "<plugin files>\hooks\hook.ps1" -EnsureRuntime`. The starter runs the installed binary and exits 0 only after `/health` answers `ok`; every protected session start runs the same script. Then repeat the `/health` request yourself and report what it printed. The first start also writes the default policy, so step 9's `/appa-tool-sync` tip has a file to work on.
   - The request prints `ok` — a runtime from an earlier install is still running. Its version is not readable over HTTP, so do not report the binary you just installed as the running one. Tell the user to stop the old process and to ask for this step again: `pkill -f appa-runtime`, on Windows `Stop-Process -Name appa-runtime`. Warn them that stopping it blocks every protected session that is already open, because the hooks fail closed.

   If the starter exits non-zero, the runtime is not running. Report that, quote the last lines of `runtime.stderr.log` in the data directory, and stop. Do not describe the setup as finished.

9. Finish by telling the user that the runtime is running and that `clappa` starts a protected session. Add a tip on the next line: run the `/appa-tool-sync` skill in the `clappa` session to build the initial security policy. Format both `clappa` and `/appa-tool-sync` as inline code.

If the `curl` download fails, ask the user to install the GitHub CLI, then try again with `gh release download`.
