# Claude Code

You run in a Claude Code session protected by APPA: the installed `appa`
binary is registered in the user's Claude Code settings as the session's
hooks, and its runtime serves the `appa` MCP server. This reference
carries the Claude Code mechanics; the router skill you came from carries
the mode and the shared rules.

The session started with advice not to read or change the policy outside
this skill. This skill is where that work happens: follow this reference
while it runs.

## Read sources

For OpenAPPA configuration, read only:

- the output of `appa describe --config <live-path>`;
- the live root config and included files relevant to the request;
- a matched battery's `appa.toml` and README;
- the relevant section of the policy-review guide the install wrote beside
  this skill, at `${CLAUDE_CONFIG_DIR:-$HOME/.claude}/skills/appa-guide/references/contracts.md`.

If the installed guide or battery files are missing or these sources do
not establish the syntax or behavior, stop and report an incomplete
installation. Do not fetch a different OpenAPPA version or search the
repository for an answer. Never search a checkout, inspect source code,
tests, Git history, or implementation details.

## Find the live config

Run `appa describe` first. Use the complete path on its `Config:` line, including
spaces. This is the installed deployment path and follows `APPA_CONFIG` and
`APPA_CONFIG_DIR` when either is set.

Then run:

```sh
ps ax -o command | grep '[a]ppa runtime'
```

If a running process visibly names a different `--config` path, stop and ask
the user which deployment to configure. Do not split an unquoted path on
spaces. If no runtime is running, continue with the path reported by `appa
describe`; initialization has already established it.

The runtime address is
`${APPA_RUNTIME_URL:-http://127.0.0.1:8787}`.

## Checkup (`init`)

### Inspect

1. List every tool this session can call: Claude Code's built-ins (`Bash`,
   `Read`, `Edit`, ...) and every MCP tool, spelled `mcp__<server>__<tool>`.
   A deferred tool counts: its name is enough.
2. Run:

   ```sh
   appa describe --config <live-path> --session-tools <name>,<name>,...
   ```

   with every name from step 1, comma-separated. Record:
   - the config state, included batteries, Authorities, audience sources,
     and named audiences;
   - `MCP servers`: every server configured on this machine or seen in this
     session;
   - the batteries that cover them, with the exact command that includes
     them and the credential each one reads;
   - `MCP servers without a battery`;
   - `Session tools`: how many tools a rule covers, which ones the Annotator
     judges call by call (each such call may ask the user), and which ones
     are refused because no rule covers them.
3. Read the root config. Record its tool rules and included batteries, and
   preserve its comments. If `appa describe` and the file disagree, stop and
   report the mismatch instead of guessing.
4. A server `appa describe` lists but whose tools this session does not see
   is configured but not inspectable here. Do not invent its tool list.

The policy names an MCP tool by its canonical id `mcp/<server>/<tool>`, split
at the first `__` after `mcp__`; a plugin-provided server is
`mcp/plugin_<plugin>_<server>/<tool>`. A Claude Code built-in is
`host/claude-code/<name>`. `appa describe` prints canonical ids. Keep each
exact tool description from the session.

The command cannot see connector accounts. The user supplies an account
identity when a connector does not expose one. Do not probe private mail,
messages, or files merely to infer an identity.

### Batteries

For each battery `appa describe` suggests, read only its `appa.toml` and
README in the deployment's store, `<config-dir>/batteries/<name>/`, where
`<config-dir>` holds the live config. Do not run its scripts. If that
directory is missing, stop and report an incomplete installation. Never
configure one APPA build with batteries fetched from another version.

When proposing a battery, give it exactly one short sentence that says what it
covers, what protection it adds, and any important assumption. Keep it under
20 words. Examples:

> Slack battery — Keeps Slack data private and asks before publishing it.
>
> GitHub battery — Assumes every repository is public and prevents private data from leaking to GitHub.

Use only the credential status reported by `appa describe`. Do not read or
print environment variables, credential files, or process environments to
check access. Name the required variable, never its value. GitHub can use `gh`
even when its token variable is unset. Never run `gh auth token` yourself;
the battery handles that internally.
Check what each battery's README expects the root config to provide, and record
anything missing. Only name a group if `appa describe` lists it as a named
audience or the proposal configures an audience source for it.

### Cover the remaining tools

Create root rules only for the tools `appa describe` reports as annotated call
by call or refused, and that no suggested battery covers. For each tool, decide
two things from its name and description:

- As a source: can someone other than the requester write the text it returns?
  Then it is `suspicious`.
- As a sink: who can end up reading what the call sends? A reader the session
  cannot see is `public`.

Apply these rules:

- The reserved `blocked` mark denies a call outright and no Authority can
  permit it; use it only where a sanitizer that would make the flow safe does
  not exist.
- A tool whose result someone other than the requester can write (a web page,
  a public issue, another session's message) uses
  `delta = { trust = "suspicious" }`.
- The built-in audience chain is `self` ⊆ `internal` ⊆ `public`: `self` is the
  person running the session, `internal` their organization.
- A tool that reads the requester's private data uses
  `delta = { audience = ["self"] }`.
- A tool that reads organization-wide data uses
  `delta = { audience = ["internal"] }`.
- Static contracts can reference `self` and `internal` without an audience
  source. Checking a literal recipient against either audience requires an
  explicit audience source.
- A tool that reads or writes one resource whose readers a source can list
  (a Slack channel, a GitHub repository, a Linear team) uses a selector
  placeholder instead of `internal`: `delta = { audience = ["@slack:channel/$channel_id"] }`
  for a read, `requires = { audience = { contains = ["@slack:channel/$channel_id"] } }`
  for a write. The spelling must match a template the provider declares under
  `selectors` on its `[externals.audience.<provider>]` binding; each
  `$argument` becomes a required string argument of the contract, so no
  `parameters` schema is needed for it. Use it whenever the matched battery
  declares such a template.
- An annotator's answer writes an audience as a static contract does: `self`,
  `internal`, an `@` mention, or a literal reader, inside its mandate's
  `audiences`. Omitted, the mandate admits every audience the policy writes.
- A tool that publishes, posts, sends, shares, or uploads beyond the machine
  requires data that may be public: `requires = { audience = { contains = ["public"] } }`.
  A destination that stays private to the requester until they share it
  themselves reaches `self` and needs no `requires`.
- A tool that communicates within the organization (e.g. posting internal Slack
  messages or workspace items) requires trusted data that includes `internal`:
  `requires = { trust = "trusted", audience = { contains = ["internal"] } }`. This
  keeps autonomous agent flow unblocked for public or internal data while preventing
  requester secrets (`self`) from leaking.
- A clearly public read or a tool whose result carries no data uses
  `delta = {}`.
- Every new tool entry needs `delta`, including entries with `requires`. Never
  fabricate reader names, groups, or audiences.

For public-audience requirements, reuse an appropriate `builtin hitl`
Authority and extend its audience permit instead of adding attention solely
to route reviews. Preserve hard denials when the operator requested them, a
root rule or comment declares them, or a mark is intentionally unserved. If
multiple Authorities can review a disclosure and the choice determines who
reviews it, ask the operator.

### Ask about ambiguity

Use tool names and descriptions when their behavior is clear. If you still
cannot tell which servers can return data that should stay private, ask the
user once. Put every unclear server in one grouped question. Do not guess and
do not ask about each tool separately.

Wait for the answer before showing the proposal. This answer does not replace
the approval required below. If nothing is unclear, do not ask.

For Gmail, match only exact tools visible in this session whose canonical ids
start with `mcp/claude_ai_Gmail/`; do not assume a fixed connector tool list. Mail
the requester reads is `self` data. Checking a named recipient against `self`
or `internal` requires an audience source. An email domain is not an audience
source: `internal` needs a directory-backed source that can enumerate its
members. Without one, say recipient-checked sends are refused as unanswerable
and leave them so. Do not invent a group. This boundary answer is separate
from approval to write or install anything.

### Propose, then apply

Open with one line on the current state: protected or not, and which batteries
are included. Then group the proposal by server. Show:

- batteries to include, each with its one-sentence explanation and the
  credential it needs;
- rules for the remaining tools, and how those tools will behave;
- existing behavior that stays unchanged, but only when it affects the result;
- tools the proposal leaves to the Annotator (judged call by call, which may
  ask the user) or refused;
- every configured MCP server whose tools could not be inspected: "<server>
  is configured, but I could not inspect its tools in this session."

At the end of the proposal, add **Needed for this to work** when any required
support is missing. Group every missing requirement there and propose the
concrete fix. For example: "Slack needs your approval before publishing, but
approval is not set up yet. I'll add it." Do not merely report "no HITL
authority," and do not mix missing requirements with unchanged rules.

Close with one plain sentence: "You can ask later to change what requires
approval or what gets blocked." Keep specific tuning options for when the
user asks for a change.

End with: **Approve, or tell me what to change.** Wait for the reply.

When nothing is missing and every session tool has a rule, say so in one or
two sentences, mention tuning in one line, and stop without approval language.

After approval:

1. Run `appa describe --config <live-path> --session-tools ...` again. If the
   config, batteries, Authorities, audience sources, or named audiences
   changed since the proposal, revise the proposal and ask for approval again.
2. Include each approved battery with the command `appa describe` printed:
   `appa battery install <name> --config <live-path>`, with
   `--server <connection-id>` when it names one. The command adds the
   battery's `appa.toml` to the root `include` list, validates the result, and
   reloads the runtime. Never copy a
   battery directory: the store beside the config already holds every battery
   of the installed version.
3. Add any root support the battery requires, such as its human-approval
   Authority. If an existing `builtin hitl` Authority handles the relevant
   attention mark but cannot review public audiences, expand its permits
   instead of adding another Authority. Do not modify an explicit hard denial.
   Describe the resulting behavior, not this wiring.
4. When the battery binds an Annotator or an audience source, name the
   variable it reads, `APPA_PROVIDER_<PROVIDER>_TOKEN` as its README
   states; it belongs in the runtime's environment, never in the config.
   Map `self` and `internal` onto the source's collections under
   `[policy.audience]` as the README shows.
5. Add the approved rules for the remaining tools to the root config. Do not
   remove overlapping root rules; they intentionally override batteries. To
   treat a battery's tool differently, add a root rule for it; never edit the
   battery.
6. Reload and report the result as described below. When the battery's
   README names a replay trace, offer
   `appa replay --config <live-path> <trace>` as the check that the
   composed config decides as the README states.

## Adjust the current config

Start from the user's requested outcome, not from a full tool rescan.

If the requested outcome is ambiguous, ask one focused question and wait. Do
not guess.

1. Run `appa describe --config <live-path>`, adding `--session-tools` with the
   tools the request is about. Record the config state, batteries, policy
   tools, Authorities, audience sources, and named audiences.
2. Read the root config and only the included files relevant to the requested
   changes.
3. For policy syntax or behavior that the current config does not demonstrate,
   first consult the relevant section of the policy-review guide at
   `${CLAUDE_CONFIG_DIR:-$HOME/.claude}/skills/appa-guide/references/contracts.md`.
   If it is unavailable or does not answer the question, stop and report an
   incomplete installation. Do not guess syntax, fetch another version, search
   for an OpenAPPA checkout, or inspect source code.
4. Explain what happens now, what you propose, and the practical effect.
   Ask only for a decision that changes the result.
5. If a battery would help, propose it with the same one-sentence rule used in
   the checkup. Existing root rules still take priority.
6. End with: **Approve, or tell me what to change.** Wait for the reply.
7. Run `appa describe --config <live-path>` again. If the config, batteries,
   Authorities, audience sources, or named audiences changed since the
   proposal, revise the proposal and ask for approval again.
8. Include each newly approved battery with
   `appa battery install <name> --config <live-path>`, as in the checkup, and
   add the root support, credential variable, and audience mapping it
   requires. To take one out, use
   `appa battery remove <name> --config <live-path>`.
9. Apply only the approved root-rule changes. To change battery behavior, add
   or edit a root rule; never modify the battery.
10. Reload and report the result as described below.

For several root rules with the same tool name, order matters. Put a narrow
argument-specific rule before its general fallback. Do not reorder unrelated
rules.

For an exact Bash command pattern, add a narrow, ordered
`host/claude-code/Bash(command:...)` root contract before the root's bare
`host/claude-code/Bash` rule. For semantic command interpretation, add a
`hint` to the root's `claude-code.bash-requirements` Annotator. Preserve its
implementation, inputs, and mandate unless the approved behavior requires a
change. Keep the root's Bash selectors above the bare Bash rule.

To make an audience mismatch reviewable, permit the intended Authority to
review that audience expansion. Do not add attention only to route the review.
Keep an existing attention requirement when it represents an independent
per-call review.

## Tune the defaults

The defaults are a middle ground: a normal coding session keeps running, and
the common ways private data leaks or outside text steers the agent are
caught. Offer these options when the user asks for a change they fit. Explain
each option's behavior and cost in plain words. Each one is a root rule or
a change to one root declaration. Mark it with a comment
`# appa-guide: <option>` so a later "undo <option>" removes exactly that.
Apply one through the `adjust` steps.

Looser:

| Option | Change | Cost |
|---|---|---|
| `trust-server <server>` | Static rules with `delta = {}` for the server's tools, in place of the Annotator's call-by-call judgment or a battery's `suspicious`. | Text other people write there reaches the session at full trust. |
| `trusted-sites <domains>` | `host/claude-code/WebFetch(url:https://<domain>/*)` with the root WebFetch rule's `requires` and `delta = {}`, placed before the root WebFetch rule. | Anyone who can edit those pages can steer the agent. |
| `instructions-after-web` | Root Write and Edit rules for `*CLAUDE.md`, `*.claude/skills/*`, `*.claude/agents/*` and `*.claude/commands/*` with `delta = {}`. | Text from a web page can end up in instructions every later session reads. |

Stricter:

| Option | Change | Cost |
|---|---|---|
| `no-fallback` | Remove the root's `name = "*"` rule and its `claude-code.undeclared-tool` Annotator. Tools no rule covers are refused. | A newly added MCP server does nothing until `/appa-guide` writes rules for it. |
| `no-leak-approvals` | Remove `audience_missing` from the `hitl` Authority's permits. A call that would send private data where a wider audience can read it is refused instead of asking. | Deliberately sharing private data needs a policy change first. |
| `private-folders <paths>` | Read, Grep and Bash selectors for the paths with `delta = { audience = ["self"] }`, before each bare rule. | After reading those folders, public sends need approval or are refused. |
| `suspicious-server <server>` | Static rules with `delta = { trust = "suspicious" }` for the server's tools. | Once the agent reads that server, writes to its own instructions and other trusted-only actions ask the user. |

When the user asks for something not in these tables, work it out from the
source and sink questions in **Cover the remaining tools**, and name its cost
the same way.

## Explain a block

1. Find the blocked call: the `[appa] Blocked` text in this conversation, or
   what the user pastes. Take the tool name, its arguments, and the `Why`
   lines. If neither is available, ask the user to paste the block.
2. Run `appa describe --config <live-path> --session-tools <tool>` to see
   whether a rule covers the tool, the Annotator judges it, or nothing covers it.
3. Read the root config and the included batteries in include order. The first
   rule whose name and argument selector match the call decides it. Selectors
   match the argument as written, case-sensitively, and `*` spans `/`.
4. In one to three sentences, say what the session had read that set its
   label, what the rule requires, and why the two differ. Name the way forward
   the block offered, if any.
5. If the user wants the call to run in future, propose the narrowest change,
   naming a **Tune the defaults** option when one fits, and continue as
   `adjust`. Otherwise stop: explaining changes nothing.

## Reload and finish

Reload only after an approved write:

```sh
curl --fail-with-body -sS -X POST \
  "${APPA_RUNTIME_URL:-http://127.0.0.1:8787}/reload"
```

The runtime checks the whole config
before installing it. If reload is refused, the previous config keeps serving.
Explain the error plainly and fix it. Ask for approval again if the fix changes
the behavior the user approved.

Briefly say what succeeded, what failed, and whether the file was restored.
If the fix changes who may receive information, say how before asking for
approval. Describe only behavior supported by the README or observed results.

After a successful reload, add:

> Start a new `clappa` session to use the updated policy; this session keeps
> the policy it started with.
