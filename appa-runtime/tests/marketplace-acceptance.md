# Marketplace installation acceptance

Run these checks with separate temporary HOME, CLAUDE_CONFIG_DIR,
APPA_INSTALL_DIR, APPA_CONFIG_DIR and APPA_DATA_DIR values. Do not use a personal
Claude profile. The native installer invokes the real `claude plugin` commands.
Disable Claude automatic updates and official marketplace auto-installation for
an offline test. No model session or provider credentials are needed.

## Native Claude and GitHub

1. Install a verified bundle with `appa plugin install claude-code --from
   <bundle> --sha256 <trusted-digest> --json`. Check that the receipt says
   registered and runtime verified, the Claude registry contains the installed
   APPA plugin, and the runtime health endpoint answers.
2. Run `appa battery install github --json`. This must compose and activate the
   policy without a second apply command or creating an MCP connection.
3. Run the installed-hook check using an environment with the MCP Python SDK:

   ```sh
   python tests/fixtures/installed-github-check.py \
     <Claude-registry-installPath> <runtime-url>
   ```

   The check reads the installed hook map and runs those commands against the
   installed runtime. It verifies an allowed GitHub identity lookup, the explicit
   remedy accepting untrusted issue content, an allowed read after that remedy,
   and a denied write from the resulting untrusted session. GitHub responses are
   fixtures: this proves installed policy enforcement, not provider connectivity
   or model behavior.
4. Export with `appa bundle --output <new-archive> --json`. Import that archive
   into a second isolated profile with a different config filename and endpoint.
   Deny external network access but allow the loopback runtime. Repeat step 3.
5. Remove GitHub from the replica with `appa battery remove github --config
   <replica-config> --json`. Verify the owned include and selection are gone,
   the runtime serves the remaining policy, and authored config and trajectory
   data remain. Retained artifacts are not deleted.
6. Run `appa plugin remove claude-code --config <replica-config> --json`.
   Verify that Claude no longer registers APPA, `clappa` is absent, and an
   unchanged APPA statusline is removed. A customized statusline must remain.
   Config, selected batteries, cached artifacts, and trajectory data remain;
   the runtime is kept. Repeating removal must report unchanged.

For interruption coverage, cause native startup to fail after config publication
(for example, deny loopback binding). Expect exit 3 and a recovery journal, never
a successful registration receipt. Restore the prerequisite and rerun the same
install; success must clear the journal and verify native activation.

## Artifact fixtures

`examples/installation_fixture.rs` creates a local, current-platform bundle from
a release-identity binary and its exact native archive. Its unused image and
platform descriptors are placeholders. It is test tooling, not a publisher, and
must never be uploaded as an official generation. Real publication uses the
`appa-package` generation example and release workflow.

When creating a native archive with macOS tar, set `COPYFILE_DISABLE=1`. Otherwise
AppleDouble metadata becomes additional regular files and correctly fails the
compiled tree identity check. The release-script round-trip regression covers
this boundary.

These native checks do not replace the kagent Python/Go deployment lanes, custom
dependency portability, explicit generation update/restore,
platform tests, or the complete feature's independent review gate.
