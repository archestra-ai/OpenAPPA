# AppenCode v1 — OpenAPPA for OpenCode V2

AppenCode checks direct OpenCode tool calls, results, and foreground subagent
returns through APPA's runtime. It targets `@opencode/cli` and
`@opencode/plugin` 2.0.28. It does not support
OpenCode V1's `@opencode-ai/plugin` API.

## Install

Install OpenCode V2 2.0.28 and the released `appa` executable. No OpenAPPA
checkout, Rust compiler, Bun installation, or plugin dependency install is required.

```sh
curl -fsSL https://openappa.com/install.sh | sh
~/.local/bin/appa plugin install opencode
```

The command retains the verified JavaScript plugin, batteries, and matching
runtime. It merges the plugin, MCP connection, and `/appa-guide` command into
your global OpenCode JSON settings. It starts the loopback runtime and verifies
its binary, deployment, and policy. Existing policy and unrelated settings remain
unchanged. Global JSONC and conflicting APPA-owned names are refused, not overwritten.

From the project whose tools you want to configure, run:

```sh
appa opencode setup
```

This opens a private operator session with your merged global and project-local
tool inventory. Only `appa_setup` reaches the setup model. Review the proposed
policy before approving publication. Close setup, run `appa plugin install opencode`
to reload the protected runtime, then start a fresh coding session. The same
install command starts the runtime after a reboot; no separate service is required.

Installation is user-global. Settings default to `~/.config/opencode/opencode.json`;
policy and runtime data use `~/.config/appa/opencode/`. Both respect `XDG_CONFIG_HOME`;
`OPENCODE_CONFIG_DIR` overrides the OpenCode settings directory. `--config` selects
another policy. Setup defaults to the current directory; optional `--project`
selects another project's inventory, not an installation scope.

Keep the deployment outside the agent's writable project. Never expose the runtime
through a public listener or portal. Its `/hook` endpoint trusts its local client.

`appa plugin remove opencode` removes only unchanged installer-owned registration.
It preserves edited entries, policies, retained artifacts, data, and the running runtime.
For offline installation, export with `appa bundle --config <policy> --output <archive>`
and restore with `appa plugin install opencode --from <archive> --sha256 <trusted-digest>`.

## Release payload

`marketplace/plugins/opencode` contains the generated JavaScript plugin and its
setup documentation. OpenCode V2 2.0.28 loads that directory directly through
`index.js`; `package.json` also exports that entry point. The operator needs
no Bun/npm install or source checkout.
The bundle includes `@opencode/schema`. OpenCode supplies the public `effect`
and `@opencode/plugin` imports. The source setup launcher is excluded from the
bundle; importing the plugin does not register settings or start setup.

Maintainers regenerate and check the committed payload with Bun 1.3.10:

```sh
(cd integrations/opencode && bun install --frozen-lockfile && bun run check)
bun scripts/appa-opencode-package.ts
bun scripts/appa-opencode-package.ts --check
```

The installer copies `default.appa.toml` into a new deployment and retains
deployment-relative `batteries/<name>/appa.toml` includes. In the private setup
session, it sets `APPA_OPENCODE_BATTERIES` to the absolute retained marketplace
`batteries` directory. The setup catalog lists all batteries declaring the
`opencode` host, not just batteries already installed in the deployment.
Publication still validates includes relative to the deployment config.
The installer materializes the version's batteries there before setup.
It sets `APPA_OPENCODE_SETUP_CONFIG` and `APPA_OPENCODE_SETUP_BINARY` only for the
operator's private setup session. The loaded plugin reads its guide and
contracts from its own shipped directory.

## Developer installation from this checkout

Build APPA and install the plugin's locked dependencies:

```sh
cargo build --locked -p appa
(cd integrations/opencode && bun install --frozen-lockfile)
```

### Guided setup

Run the setup launcher from the project whose tools you want to inspect:

```sh
bun run /absolute/path/to/OpenAPPA/integrations/opencode/setup.ts
```

Registration and policy are user-global. Setup preserves existing global JSON
settings, registers AppenCode and its MCP connection, adds `/appa-guide`, and
creates a policy that includes the OpenCode battery. It does not write project
settings. Existing policies remain unchanged.

The default paths are `~/.config/opencode/opencode.json` for OpenCode settings
and `~/.config/appa/opencode/appa.toml` for policy. Both respect `XDG_CONFIG_HOME`;
`OPENCODE_CONFIG_DIR` overrides the OpenCode directory. Use `--config` for a
different policy path. Projects using this deployment share its policy and
runtime, so approved changes affect all of them.

Setup uses the current project by default. `--project /path/to/project` selects
another project's tool inventory; it does not select an installation scope.
OpenCode loads project-local settings alongside the global settings. The setup
session retains those project-local providers and MCP servers.
Keep this checkout and the deployment outside the agent's writable project.
The installed configuration references this checkout's battery files.

The launcher opens a fresh, private OpenCode server for operator setup. The
agent inspects the actual tool registry, reads the policy and relevant batteries,
then proposes changes. Review the proposal and approve it in a later message.
The agent validates the complete configuration before saving it. Validation
errors and stale proposals leave the existing policy unchanged.

Inspection and publication wait up to 10 seconds for pending MCP discovery
across global and project servers. Inspection reports server statuses alongside
the observed tools. Publication refuses pending, failed, or authentication-required
connections; disabled servers are excluded. Connected status does not prove
registry completeness in OpenCode 2.0.28. Inspection and publication therefore
report observed inventory, not complete tool coverage. Inspect again if a newly
connected server's tools have not appeared. Configuration validation does not
prove that every configured tool was discovered.

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
runtime's loopback HTTP origin. Existing conflicting global MCP or command
entries are refused rather than overwritten. Global JSONC settings require
manual setup; project JSONC files remain untouched and load normally.

The OpenCode battery starts conservatively: private reads narrow audience,
web results lower trust, edits require trust, and the shell permits only `pwd`.
The guide proposes additional root contracts for the tools you need. It includes
only batteries that declare OpenCode support: Microsoft Learn, Cloudflare,
Notion, and GitHub, in addition to the OpenCode baseline.
Other catalog batteries are not automatically treated as OpenCode-compatible.

### Service batteries

Connect the service's MCP server in OpenCode first. Battery inclusion supplies
policy, not a connection or its authentication. The setup guide uses the live
registry to propose `aliases.mcp` bindings when connection names differ from
battery namespaces. Cloudflare can bind both documentation and observability
connections to its one namespace.

Map `internal` to the people who may read everything a Cloudflare observability
or Notion connection reaches. Notion's structural and agent-session changes
also require an Authority permitting `notion-review`; ordinary trusted workspace
writes do not. The guide proposes this support with the battery, subject to
operator approval. It does not add a wildcard Authority.

Use OpenCode's terminal UI for human review. Version 2.0.28 exposes MCP review
forms there, but not in its web/desktop session interface. Review shows the exact
call. **Yes is initially highlighted; Enter submits approval.** Select No to
refuse, or Escape to dismiss without a ruling. A missing answer grants nothing.
Forms belong to the project's global queue, so inspect the call rather than
assuming it belongs to the currently displayed session. In testing, newly queued
forms sometimes did not appear until the terminal reconnected. If a remedy waits
without showing a form, reconnect a server-attached terminal to the same project,
leaving the OpenCode server running. Do not restart the server to surface a review.

GitHub's Python helpers need their own credentials in the protected runtime's
environment. For `single-user`, use `APPA_PROVIDER_GITHUB_TOKEN` or a `gh` login;
run `python3 check.py` from the installed GitHub battery directory to check access.
For `multi-user`, use a GitHub App installation token and `python3 check-multi-user.py`.
The MCP connection's authentication does not provision these helpers. Use the same
`APPA_BATTERY_TARGET` for setup and the protected runtime; it defaults to
`single-user`. Setup shows the selected target policy and validates its composition
with the base policy. Include only `appa.toml`; the runtime adds the selected target
automatically. **Successful publication does not establish credential readiness
or repository access.**

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

Merge the following entries into the user's global OpenCode settings,
normally `~/.config/opencode/opencode.json`, preserving other settings. Respect
`XDG_CONFIG_HOME` and `OPENCODE_CONFIG_DIR` when set. Replace the absolute plugin
directory with this checkout's `integrations/opencode` directory.
V2 uses `plugins`, not `plugin`:

```json
{
  "plugins": ["/absolute/path/to/OpenAPPA/integrations/opencode"],
  "mcp": {
    "servers": {
      "appa": { "type": "remote", "url": "http://127.0.0.1:8789/mcp", "oauth": false, "codemode": false }
    }
  }
}
```

Set `codemode` to `false` so OpenCode does not instruct the model to use
the disabled `execute` tool. AppenCode exposes MCP tools directly. Use the
same setting for other MCP servers connected to this protected deployment.

Keep the exact MCP server name `appa`. Before admitting a protected prompt,
AppenCode waits up to 10 seconds for its real `appa_execute_remedy_plan` executor
to appear in the direct-tool registry. Unavailable connections or timeout reject
the prompt before inference. Check the connection and retry the prompt; AppenCode
does not reconnect or enable servers automatically. Unrelated MCP servers do not
delay this check. This covers submitted prompts, not every automatic continuation
or later disconnection. Discovery does not execute a remedy; its tool call must
execute successfully before retrying the refused flow.
OpenCode 2.0.28 returns HTTP 500 for prompt-hook failures without the diagnostic
body. Check its logs and `opencode mcp list` in the same project and server.
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

### Published CLI smoke check

The CLI smoke check runs separately from `bun test`. It loads the committed
marketplace bundle, not the TypeScript source. Build APPA, then run:

```sh
(cd integrations/opencode/smoke && bun install --frozen-lockfile)
python3 integrations/opencode/smoke/run.py
```

The check uses the published CLI and host plugin SDK at the same pinned version.
A local OpenAI-compatible SSE fixture scripts tool proposals. No model provider
credentials, external inference, or paid tokens are used. Native OpenCode tools,
APPA decisions, and MCP remedy execution are real.

One root attempts a private read, executes its offered remedy, retries the read,
and attempts a public web request. The check requires ordered native CLI tool
results and zero requests at a local receiver. Receiver controls run before and
after the journey. Assistant prose cannot satisfy these assertions.

The runner owns an isolated OpenCode server. It waits for APPA health, MCP
connection, and the remedy tool's actual advertisement before starting the
journey. This checks CLI/host integration, not standalone cold-start behavior.
Each run uses fresh HOME/XDG state, synthetic data, and an APPA policy/database
outside the project. The runner copies the starting policy above unchanged.

`--output /path/to/new-directory` selects where to retain diagnostic evidence;
otherwise the runner creates a temporary directory and prints its path. Use
`APPA_TEST_BINARY` to select a different APPA build. CI runs the check in
`Runtime Binary Checks` and uploads failure evidence for seven days. Keep CLI
and SDK pins aligned when upgrading. Video rendering is not part of this check.

Check the example policy without invoking tools:

```sh
./target/debug/appa describe --adapter opencode --config integrations/opencode/appa.toml --check
```

Without an observed inventory, tool availability is unknown. `--session-tools`
accepts canonical identities such as `appa/execute_remedy_plan`; a flattened
MCP dispatch name alone does not tell this CLI its namespace.
