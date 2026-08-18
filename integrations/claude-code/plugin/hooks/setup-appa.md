This Claude Code session is not protected by OpenAPPA. The OpenAPPA plugin is installed, but the `appa-runtime-v2` binary is not.

At the start of the session, tell the user in one short sentence: the Claude + APPA beta is available, and you can set it up for them if they want. Say it once, do not repeat it, and do not install anything without being asked.

When the user asks for the setup, install the runtime:

1. Pick the release archive for this system: `appa-runtime-v2-<arch>-<os>.tar.gz`, where `<arch>` is `aarch64` or `x86_64` (`uname -m`) and `<os>` is `apple-darwin` or `unknown-linux-gnu` (`uname -s`). On native Windows it is `appa-runtime-v2-<arch>-pc-windows-msvc.zip`.
2. Download the archive plus `SHA256SUMS` and `version.txt` into a temporary directory: `curl -fsSL -O https://github.com/archestra-ai/OpenAPPA/releases/latest/download/<asset>` for each of the three assets. No authentication is needed. `gh release download --repo archestra-ai/OpenAPPA --pattern <asset>` works too when `gh` is already installed.
3. Verify before anything runs: the archive's SHA-256 must equal its line in `SHA256SUMS` (`shasum -a 256` or `sha256sum`). On a mismatch, stop and tell the user; do not install.
4. Extract the archive and check the binary: `./appa-runtime-v2 --version` must print exactly `appa-runtime-v2 <contents of version.txt>`.
5. Install the binary to the install target named at the top of this context, mode 755, creating the directory when needed. That exact path is where the plugin's hooks look for it; do not choose a different location. Do not start it: protected sessions start it on demand. When reporting this step, do not say "not started" as if something is missing — say the runtime will be started when `clappa` is called, formatting `clappa` as inline code.
6. Create the `clappa` command as an executable, not an alias, so it works in every open terminal with no shell reload: write `clappa` into the same directory as the runtime binary, mode 755, containing:

   ```sh
   #!/bin/sh
   exec env APPA_GATE=1 claude "$@"
   ```

   Only if that directory is not on the user's `PATH`, fall back to appending `alias clappa='APPA_GATE=1 claude'` to the file matching their shell and tell them to reload it. On Windows, add the matching `clappa` function to the PowerShell profile.

7. Install the statusline, unless `~/.claude/settings.json` already has a `statusLine` entry — never replace one. Copy `statusline.sh` (on Windows, `statusline.ps1`) from the plugin files directory named at the top of this context into the runtime binary's directory as `appa-statusline.sh`, mode 755, and merge into `~/.claude/settings.json`:

   ```json
   {"statusLine": {"type": "command", "command": "<that path>/appa-statusline.sh"}}
   ```

   It shows the APPA mascot with the session's trust and audience when protected, and a `clappa` reminder when not.

8. Finish by telling the user to start a protected session with `clappa`. Add a tip on the next line: run the `/appa-tool-sync` skill in the `clappa` session to build the initial security policy. Format both `clappa` and `/appa-tool-sync` as inline code.
