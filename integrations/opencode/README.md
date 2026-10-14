# AppenCode v1 — OpenAPPA for OpenCode V2

AppenCode checks direct OpenCode tool calls, results, and foreground subagent
returns through APPA's runtime. It targets `@opencode/cli` and
`@opencode/plugin` 2.0.28. It does not support
OpenCode V1's `@opencode-ai/plugin` API.

## Install from this checkout

Build APPA and install the plugin's locked dependencies:

```sh
cargo build --locked -p appa
(cd integrations/opencode && bun install --frozen-lockfile)
```

### Guided setup

Run the setup launcher from this checkout:

```sh
bun run integrations/opencode/setup.ts --project /absolute/path/to/your-project
```

It preserves existing JSON settings, registers AppenCode and its MCP connection,
adds `/appa-guide`, and creates a policy that includes the OpenCode battery.
Existing policies remain unchanged. The default deployment is
`~/.config/appa/opencode/appa.toml`; use `--config` for another path.
Keep this checkout and the deployment outside the agent's writable project.
The installed configuration references this checkout's battery files.

The launcher opens a fresh, private OpenCode server for operator setup. The
agent inspects the actual tool registry, reads the policy and relevant batteries,
then proposes changes. Review the proposal and approve it in a later message.
The agent validates the complete configuration before saving it. Validation
errors and stale proposals leave the existing policy unchanged.

Only `appa_setup` is exposed to the setup model. A separate APPA runtime gates
that tool; ordinary shell, file, MCP, and delegation calls are not authorized.
The tool can inspect the fixed deployment, read catalog batteries, and publish
its root policy. It cannot choose another target or run arbitrary commands.
Approval is an agent instruction, not a separately authenticated human decision.
Treat setup as an administrative session and close it before coding. Never set
`APPA_OPENCODE_SETUP_CONFIG` or `APPA_OPENCODE_SETUP_BINARY` on ordinary servers.

After setup, start or restart the protected runtime using the command printed
by the launcher, then start a fresh OpenCode session in the project. Setup saves
the policy; it does not reload an existing protected runtime. In an Amp orb,
use the supervised runtime command below. Do not reuse the setup conversation
for protected work.

Run `/appa-guide` in a normal session to get the operator setup command for
later changes. Use `--install-only` to write configuration without opening
OpenCode. `--binary` selects an APPA binary; `--runtime-url` selects the protected
runtime's loopback HTTP origin. Existing conflicting MCP or command entries
are refused rather than overwritten. JSONC configurations require manual setup.

The OpenCode battery starts conservatively: private reads narrow audience,
web results lower trust, edits require trust, and the shell permits only `pwd`.
The guide proposes additional root contracts for the tools you need. It includes
only batteries that declare OpenCode support; Microsoft Learn is also supported.
Other catalog batteries are not automatically treated as OpenCode-compatible.

### Manual setup

Keep the deployment policy and database outside the agent's writable project.
Copy the starting policy without replacing existing configuration:

```sh
mkdir -p "$HOME/.config/appa/opencode"
test -e "$HOME/.config/appa/opencode/appa.toml" || \
  cp integrations/opencode/appa.toml "$HOME/.config/appa/opencode/appa.toml"
```

Review the policy before use. It labels reads under `private/` as internal,
requires public audience for web requests, and requires trust for edits.
It permits only `pwd` through the shell. This is an example, not a
general-purpose coding policy. Declare other tools or register an Annotator.
Undeclared tools are refused. Other shell commands fail with
`arguments match no registered contract`; this is an expected policy refusal.

Start the runtime and leave it running:

```sh
./target/debug/appa runtime --adapter opencode --listen 127.0.0.1:8789 \
  --config "$HOME/.config/appa/opencode/appa.toml" \
  --db "$HOME/.config/appa/opencode/appa.db"
```

In an Amp orb, use a supervised service:

```sh
amp orb service start appencode-runtime --command "\"$(pwd)/target/debug/appa\" runtime --adapter opencode --listen 127.0.0.1:8789 --config \"$HOME/.config/appa/opencode/appa.toml\" --db \"$HOME/.config/appa/opencode/appa.db\""
```

Do not expose the runtime through a portal. `/hook` trusts its local client
and does not authenticate callers. Keep the database across restarts.

Merge the following entries into the target project's `opencode.json`,
preserving other settings. Replace the absolute plugin directory with this
checkout's `integrations/opencode` directory. V2 uses `plugins`, not `plugin`:

```json
{
  "plugins": ["/absolute/path/to/OpenAPPA/integrations/opencode"],
  "mcp": {
    "servers": {
      "appa": { "type": "remote", "url": "http://127.0.0.1:8789/mcp", "oauth": false }
    }
  }
}
```

Keep the exact MCP server name `appa`. Connect it before protected work;
the agent needs its `appa_execute_remedy_plan` tool to execute remedy plans.
MCP discovery can finish after the first model request. If the tool is not yet
available, wait for connection and retry; a remedy has not executed until its
tool call succeeds.
Install the plugin in a host-controlled directory. A plugin inside the
agent's writable project is not protected against modification.

Start a **new session** after configuration reload. Existing conversation
content is not retroactively labeled. Set `APPA_OPENCODE_RUNTIME_URL` in
the OpenCode server's environment to change the hook endpoint. Update
`mcp.servers.appa.url` to the same runtime's `/mcp` endpoint as well.
V2 uses a background
server; restarting only the CLI does not refresh that server's environment.
Use `opencode run --standalone --auto 'your task'` for a private-server run.
`--auto` does not override explicit permission denials or APPA decisions.

## Tool identities and remedy plans

| OpenCode dispatch name | APPA policy identity |
| --- | --- |
| `read`, `shell`, `write`, `edit`, `webfetch` | `host/opencode/<name>` |
| `appa_subagent` | `host/opencode/appa_subagent` (single child launch) |
| Namespaced tool, such as `github_create_issue` | `mcp/github/create_issue` |
| `appa_execute_remedy_plan` from namespace `appa` | `appa/execute_remedy_plan` |
| Unnamespaced lookalike `appa_execute_remedy_plan` | `host/opencode/appa_execute_remedy_plan` |

The plugin obtains names and namespaces from the tool registry. It does not
split flattened dispatch names on underscores. Namespaced plugin tools use
the same namespace mapping as MCP tools. Ambiguous wire spellings are refused.
The runtime validates the observed inventory and keeps identities stable
within a session.

A private read first offers a remedy to accept a narrower audience. The agent
calls `appa_execute_remedy_plan` with the offered `offer_id`, then retries.
Executing the plan does not perform the read. Public web requests remain
blocked after private content enters the trajectory. This example's `pwd`
contract also requires public audience, so it stops after that narrowing.
Web results require accepting suspicious trust; subsequent trusted-only edits
then stop. Sanitizers can replace
confined results when the deployment registers them. The starting policy
registers no sanitizer.

## Foreground subagents

The plugin exposes `appa_subagent` for fresh, one-level foreground delegation.
The starting policy declares `host/opencode/appa_subagent` and enables
`policy.deployment.context_control`. The agent prepares a return contract by
executing the offered remedy, then retries the same delegation arguments.
Each call needs its own declaration, including concurrent calls.

The tool accepts `agent`, `description`, and `prompt`, plus an optional `model`
override. Select a visible subagent, not a primary agent. A model override
must identify an available provider, model, and optional variant. Existing
`sessionID` values and `background: true` are refused.

APPA binds the fresh child to the exact launching call before its initial
prompt. The child inherits the parent's Label and receives its return
contract through the context hook. Its direct tools use the same APPA family,
with a distinct child identity. Only a completed final text answer can return.
APPA admits that value, or its sanitizer replacement, before the parent sees
it. The plugin constructs fresh output and text channels from the admitted
value; it does not forward child attachments, metadata, or raw errors.

Launch binding is serialized within each parent. Bound children can run
concurrently and finish in any order. Interruption revokes child ownership,
interrupts the child, and attempts to settle the launch without returning its
prose. A failed child cannot return earlier prose. Uncertain outcomes remain
unsettled if the runtime cannot acknowledge cleanup.

An APPA restart with the same database preserves a live child's binding and
inherited Label. An OpenCode restart loses local ownership: old children
cannot resume or return. A reloaded plugin cannot adopt existing children.
Steering, nested delegation, child
compaction, auxiliary generation, and alternate child launches are refused.

**Delegation uses APPA authorization, not OpenCode's native `subagent`
permission rule.** The public V2 SDK does not expose the native permission
assertion to this tool. The native tool stays removed and denied. Native
foreground Jobs can become background Jobs and deliver delayed synthetic
messages outside the mutable tool-result hook. The owned executor registers
neither a Job nor a synthetic completion observer. OpenCode's background
action therefore cannot convert its foreground return into a delayed message.

## V1 boundaries

- Direct tool calls fail closed on connection errors, timeouts, invalid replies,
  and runtime refusals. Unchecked results are replaced in every result channel:
  structured output, text, attachments, metadata, and error causes.
- **Code Mode is refused.** Its built-in `fetch` has no public admission hook.
  Registered tools are exposed directly instead. Native subagents, unowned
  child sessions, and forked sessions are refused. Use `appa_subagent` for
  foreground delegation. Continue the protected root rather than copying its
  history into a fork.
- **Reads keep OpenCode's native permissions.** The plugin resolves the requested
  path before policy selection. Missing paths are refused instead of allowing
  the native reader to choose an alternate filename. Reads that could load
  additional nearby `AGENTS.md` instructions are refused before execution.
  Initial project instructions remain trusted harness input, outside tool gating.
- **File-label tracking is deferred.** V1 does not track native file versions
  or attach Labels to writes. A later read in another session uses its policy
  contract, not the writing session's Label.
- **Labels stay in APPA's database.** Compaction, revert, plugin reload, and
  runtime restart do not clear a root's Label. Revert restores files but cannot
  erase an earlier flow within that root.
- **Inference is not gated.** Main, title, compaction, and generation requests,
  provider-executed tools, assistant prose, sharing, and attachments outside
  tool results are outside this boundary. Providers and configured
  provider-side capabilities must be trusted.
- **Background shell requests are refused.** Host-initiated backgrounding,
  delayed notifications, direct session shell commands, and other host/plugin
  side effects are outside tool-result gating. Missing outcomes remain
  unsettled in APPA; interruption does not prove that a tool had no effects.
- **This is not an OS sandbox.** Native tools, MCP servers, plugins, formatters,
  and shell subprocesses retain their host capabilities. Path checks do not
  establish filesystem confinement or prevent concurrent filesystem changes.
  General shell access requires separate confinement and appropriate contracts.
- OpenCode, its plugin host, installed plugins, and APPA are trusted. Hooks run
  in registration order. A later plugin can change an approved call before
  execution; AppenCode can detect changed arguments at result admission, but
  cannot undo side effects. Plugin unload removes protection.

The issue [OpenAPPA #590](https://github.com/archestra-ai/OpenAPPA/issues/590)
informed the adapter layout. Its V1 tool names and proposed spawn mapping are
not the V2 contract. The
[OpenCode sandbox plugin](https://github.com/isanchez31/opencode-sandbox-plugin)
provides useful shell-sandbox design, but its V1 `bash` hooks do not run under
this V2 API and it does not mediate native file tools. It is not an AppenCode
dependency or a claimed security boundary.

To uninstall, remove the plugin entry and reload OpenCode. Stop the runtime
separately. Keep the policy and database if protected sessions will resume.

## Future work

The restrictions above are integration limits, not judgments that these
OpenCode features are undesirable. The APPA engine already supports child
trajectories and checked returns; AppenCode maps fresh foreground children.
Keep a feature refused until its additional flows are mediated. An OpenCode
permission approval alone must not bypass that boundary.

The [plugin](./appencode.ts) owns lifecycle mapping; the
[adapter](../../appa-adapter-opencode/src/lib.rs) owns spawn and tool identities.
The [Claude Code mapping](../../appa-adapter-claude-code/src/parse.rs) shows
how an existing integration reports child starts and checked returns.

1. **Child resume and nested delegation.** Preserve the launching call and
   APPA family identity when reusing a child. Admit additional prompts and
   their sources before the child works. Map nested parentage explicitly;
   OpenCode's immediate `parentID` is not the APPA family root. Persist local
   ownership safely before allowing child recovery after an OpenCode restart.
2. **Conversation forks.** Map OpenCode's fork lineage to APPA's root-fork
   operation. A conversation copied from a protected parent must inherit its
   security state, not open as a neutral root. Cover resume, revert, and
   restart; copied history must not lose its restrictions.
3. **Background results and alternate child launches.** Mediate delayed child
   and shell completion messages, including foreground jobs moved into the
   background. OpenCode admits these through synthetic messages rather than
   the original tool-result hook. Commands can also create children without
   the subagent tool. Observing completion after delivery is insufficient:
   the integration needs a boundary that can withhold or replace what reaches
   the parent. Public hook coverage may need an upstream extension.
4. **Code Mode.** In the targeted 2.0.28 implementation, its host-side `fetch`
   bypasses individual tool hooks. Mediate those network emissions and assign
   distinct APPA call identities to nested tool calls before enabling
   `execute`. Checking the final result cannot undo an earlier HTTP request.
   Reassess the actual host implementation when changing the pinned version.
5. **Implicit instructions and model requests.** Admit nested `AGENTS.md`
   content before OpenCode adds it to the conversation, instead of refusing
   the surrounding read. Evaluate the V2 model-request hooks separately for
   main, title, compaction, and generation requests, including WebSocket
   providers. Tool-result protection alone does not mediate these emissions.
6. **File-label tracking: deferred, outside the first subagent extension.**
   When this work resumes, integrate APPA's runtime-owned file mediation
   rather than attaching a Label only to a pathname. Preserve Labels across
   writes, later reads, and snapshot restoration, and prevent native tools or
   subprocesses from bypassing the managed workspace. Foreground subagent
   support does not depend on completing this work.

For each enabled path, test both permitted and refused flows through the
published OpenCode CLI and real APPA runtime. Verify that private child data
cannot reach a public parent result, sanitized bytes replace every result
channel, and interruptions or restarts cannot erase inherited restrictions.

## Verify

From the checkout:

```sh
cargo test --locked -p appa-adapter-opencode -p appa-runtime-api -p appa-package
cargo build --locked -p appa
(cd integrations/opencode && bun install --frozen-lockfile && bun run check && bun test)
```

The tests include a host-shaped driver and a real published OpenCode 2.0.28
server. Both run the real APPA daemon and MCP endpoint with disposable
policies and databases. The server tests script only model inference; plugin
loading, native tools, session lifecycle, and remedy execution are real.
They check inherited audience/trust, return floors, sanitizer replacement,
concurrent identities, interruption, restart, and refused alternate launches.
They also check ordinary root flows and replacement of all tool-result
channels. No inference-policy enforcement is claimed.

Check the example policy without invoking tools:

```sh
./target/debug/appa describe --adapter opencode --config integrations/opencode/appa.toml --check
```

Without an observed inventory, tool availability is unknown. `--session-tools`
accepts canonical identities such as `appa/execute_remedy_plan`; a flattened
MCP dispatch name alone does not tell this CLI its namespace.
