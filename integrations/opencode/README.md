# AppenCode v1 — OpenAPPA for OpenCode V2

AppenCode checks direct OpenCode tool calls and results through APPA's runtime.
It targets `@opencode/cli` and `@opencode/plugin` 2.0.28. It does not support
OpenCode V1's `@opencode-ai/plugin` API.

## Install from this checkout

Build APPA and install the plugin's locked dependencies:

```sh
cargo build --locked -p appa
(cd integrations/opencode && bun install --frozen-lockfile)
```

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

## V1 boundaries

- Direct tool calls fail closed on connection errors, timeouts, invalid replies,
  and runtime refusals. Unchecked results are replaced in every result channel:
  structured output, text, attachments, metadata, and error causes.
- **Code Mode is refused.** Its built-in `fetch` has no public admission hook.
  Registered tools are exposed directly instead. Subagents, child sessions,
  and forked sessions are refused because v1 has no checked child-return or
  root-fork mapping. Continue the protected root rather than copying its history.
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
trajectories and checked returns; AppenCode does not yet map that lifecycle.
Keep a feature refused until its additional flows are mediated. An OpenCode
permission approval alone must not bypass that boundary.

The [plugin](./appencode.ts) owns lifecycle mapping; the
[adapter](../../appa-adapter-opencode/src/lib.rs) owns spawn and tool identities.
The [Claude Code mapping](../../appa-adapter-claude-code/src/parse.rs) shows
how an existing integration reports child starts and checked returns.

1. **Foreground subagents first.** Bind each child to its launching call and
   inherit the parent's Label before the child works. Prepare its return
   contract before delegation, check its tool flows, and admit or sanitize its
   answer before the parent receives it. Map V2's child `session.context`
   hooks, lineage from `session.get`, and mutable foreground results in
   `tool.execute.after` to APPA's child lifecycle. Cover concurrent launches,
   resumed children, cancellation, and restart without confusing identities.
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

The tests use the published V2 SDK and a host-shaped driver with the exported
plugin. They also run the real APPA daemon and MCP endpoint with disposable
policies and databases. They check remedies, audience/trust denials, full
result replacement, sanitizer delivery, parallel outcomes, and restart
persistence. Mock-hook tests alone do not prove that OpenCode loads the plugin
or delivers the hooks. No inference-policy enforcement is claimed.

Check the example policy without invoking tools:

```sh
./target/debug/appa describe --adapter opencode --config integrations/opencode/appa.toml --check
```

Without an observed inventory, tool availability is unknown. `--session-tools`
accepts canonical identities such as `appa/execute_remedy_plan`; a flattened
MCP dispatch name alone does not tell this CLI its namespace.
