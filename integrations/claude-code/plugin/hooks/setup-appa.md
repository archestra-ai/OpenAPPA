This Claude Code session is not gated by OpenAPPA. The OpenAPPA plugin is installed, but the `appa-runtime-v2` binary is not.

At the start of the session, tell the user in one short sentence: the Claude + APPA beta is available, and you can set it up for them if they want. Say it once, do not repeat it, and do not install anything without being asked.

When the user asks for the setup, install the runtime:

1. Pick the release archive for this system: `appa-runtime-v2-<arch>-<os>.tar.gz`, where `<arch>` is `aarch64` or `x86_64` (`uname -m`) and `<os>` is `apple-darwin` or `unknown-linux-gnu` (`uname -s`). On native Windows it is `appa-runtime-v2-<arch>-pc-windows-msvc.zip`.
2. Download the archive plus `SHA256SUMS` and `version.txt` into a temporary directory: `gh release download --repo archestra-ai/OpenAPPA --pattern <archive> --pattern SHA256SUMS --pattern version.txt`. While the repository is private this needs an authenticated `gh`; once public, `curl -fsSL https://github.com/archestra-ai/OpenAPPA/releases/latest/download/<asset>` works too.
3. Verify before anything runs: the archive's SHA-256 must equal its line in `SHA256SUMS` (`shasum -a 256` or `sha256sum`). On a mismatch, stop and tell the user; do not install.
4. Extract the archive and check the binary: `./appa-runtime-v2 --version` must print exactly `appa-runtime-v2 <contents of version.txt>`.
5. Install the binary to the install target named at the top of this context, mode 755, creating the directory when needed. That exact path is where the gate's hooks look for it; do not choose a different location. Do not start it: gated sessions start it on demand.
6. Add the gate alias to the user's shell configuration unless it is already there: append `alias clappa='APPA_GATE=1 claude'` to `~/.zshrc`, `~/.bashrc`, or the file matching their shell (on Windows, the matching `clappa` function to the PowerShell profile). Then tell the user to reload their shell and start a gated session with `clappa`.

If `gh` is missing or unauthenticated, tell the user to install the GitHub CLI and run `gh auth login` first, then ask again.
