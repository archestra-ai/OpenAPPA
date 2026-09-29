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
- a matched battery's `appa.toml`, `appa-package.toml`, and README;
- `appa battery status --config <live-path> --json` for credential and dependency status;
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

For each battery `appa describe` suggests, read only its `appa.toml`, `appa-package.toml`, and
README in the deployment's store, `<config-dir>/batteries/<name>/`, where
`<config-dir>` holds the live config. Do not run its scripts directly; use the bounded `appa battery status --check` readiness checks. If that
directory is missing, stop and report an incomplete installation. Never
configure one APPA build with batteries fetched from another version.

When proposing a battery, give it exactly one short sentence that says what it
covers, what protection it adds, and any important assumption. Keep it under
20 words. Examples:

> Slack battery — Keeps Slack data private and asks before publishing it.
>
> GitHub battery — Assumes every repository is public and prevents private data from leaking to GitHub.

Name each credential variable `appa describe` reports. Inspect its effective
source with `appa battery status --config <live-path> --battery <name>,<name> --json`.
An environment value takes precedence over a saved database value. A missing
variable can still work through a battery's declared CLI authentication fallback.
Use `--check` to verify that fallback; executable presence alone is not login.
Check what each battery's README expects the root config to provide, and record
anything missing. Only name a group if `appa describe` lists it as a named
audience or the proposal configures an audience source for it.

### Develop the Bash classifier hint

During `init`, develop a concise `hint` for the root
`claude-code.bash-requirements` Annotator from the contracts of every battery
that the approved configuration will include. This lets Claude apply the same
policy intent when a CLI command reaches a service covered by an MCP battery.

Read the batteries' `appa.toml` and README as source material. Translate their
source and sink intent for recognizable CLI usage. Give the classifier a short
instruction and authority to reason from the complete command; do not build an
exhaustive command list or restate OpenAPPA's label guide. Keep the complete
hint within the documented 512-character limit.

This translation is not equivalent MCP enforcement. A Bash call does not gain
an MCP tool's typed arguments, Annotator, audience source, or knowledge of its
readers. Use a battery's dynamic facts only when an installed context provider
actually supplies them for that Bash call. Otherwise, do not invent visibility,
reader membership, account identity, or other runtime facts. State uncertainty
conservatively in the hint when the command and supplied context cannot
establish the boundary.

Keep specialized Annotators separate. In particular, the GitHub battery
supplies `context.github` to the existing
`claude-code.bash-repository-requirements` Annotator; do not replace it, merge
it into the generic Bash Annotator, or claim that a generated hint reproduces
it. Preserve every Annotator's implementation, inputs, and mandate.

Treat an existing root hint as an operator customization. Preserve it and add
compatible battery-derived guidance only when the combined hint remains
concise and within the limit. If the guidance conflicts or does not fit,
propose the smallest explicit revision instead of silently replacing it.

Include the exact proposed hint text and its practical effect in the proposal.
Do not write it before approval. If the resulting hint is unchanged, propose no
hint edit. After approval, copy the complete existing
`claude-code.bash-requirements` declaration into the root only when needed,
change only its `hint`, and leave the included battery files unchanged.

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
- the exact Bash classifier hint derived from the batteries, when it changes,
  and what CLI behavior it adds;
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
two sentences, mention tuning in one line, and finish with **Review in the
browser** below, without approval language.

After approval:

1. Run `appa describe --config <live-path> --session-tools ...` again. If the
   config, batteries, Authorities, audience sources, or named audiences
   changed since the proposal, revise the proposal and ask for approval again.
   Re-read the root config and compare the complete
   `claude-code.bash-requirements` declaration, including its `hint`, with the
   version used for the proposal. If it changed, preserve the new declaration,
   revise the proposal, and ask for approval again.
2. Resolve all approved batteries' missing prerequisites together before including
   them. Run `appa battery status --config <live-path> --battery <name>,<name> --json --check`.
   If credentials, CLI login, or required executables need configuration, serve one
   consolidated browser page and give the user its link:

   ```sh
   appa ui --config <live-path> --setup --battery <name>,<name> --no-open
   ```

   Pass every proposed battery in that one command; never serve one page per token.
   The link opens Batteries with `?configure=true`, checking battery readiness and expanding only batteries that need configuration.
   Ready batteries stay collapsed; do not ask the user to configure them again.
   The user enters tokens directly into the browser and chooses **Save and check**.
   Never ask for tokens in chat, read the credential database, or put token values
   in shell commands or configuration files. Login hints are instructions for the
   user, not commands to execute automatically. Never open a browser: show the
   URL the command printed as a link. The command serves the page until it is stopped,
   so run it in the background and stop it after the user finishes. It works whether
   or not the runtime is running. Saving reloads a running runtime; if none is
   running, the next session starts it with the saved credentials.
   After the user finishes, rerun the combined status command with `--check` and
   use only its sanitized results. A `ready` result with reason `configured` means
   the battery's executables exist and its tokens are set, but no provider check
   ran; do not claim its provider access was tested.
3. Include each approved battery with the command `appa describe` printed:
   `appa battery install <name> --config <live-path>`, with
   `--server <connection-id>` when it names one. The command adds the
   battery's `appa.toml` to the root `include` list, validates the result, and
   reloads the runtime. Never copy a
   battery directory: the store beside the config already holds every battery
   of the installed version.
4. Add any root support the battery requires, such as its human-approval
   Authority. If an existing `builtin hitl` Authority handles the relevant
   attention mark but cannot review public audiences, expand its permits
   instead of adding another Authority. Do not modify an explicit hard denial.
   Describe the resulting behavior, not this wiring.
5. When the battery binds an Annotator or an audience source, name the
   variable it reads, `APPA_PROVIDER_<PROVIDER>_TOKEN` as its README
   states; the helper receives it through its environment, supplied by the runtime's
   environment or local credential database, never the policy config.
   Map `self` and `internal` onto the source's collections under
   `[policy.audience]` as the README shows.
6. Add the approved rules for the remaining tools to the root config. Do not
   remove overlapping root rules; they intentionally override batteries. To
   treat a battery's tool differently, add a root rule for it; never edit the
   battery. Immediately before changing the Bash hint, re-read the root and
   replace only the exact complete `claude-code.bash-requirements` declaration
   used in the approved proposal. If it no longer matches, stop, preserve the
   current declaration, and revise the proposal instead of overwriting it.
7. Reload and report the result as described below. When the battery's
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
8. Resolve all newly approved batteries' prerequisites using the consolidated
   browser setup flow above, then include each newly approved battery with
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

## Review in the browser

End every `init` and `adjust` run here, after a successful reload or a
no-change result. Do not do this after a refused reload or in `explain`.

Serve the policy overview in the background, without opening a browser:

```sh
appa ui --config <live-path> --no-open
```

The page shows each MCP server the policy covers, where each rule comes from
(the root config or a battery), and each server's contracts. Tell the user in
one sentence to review the policies there, and give the URL the command
printed as a link. Never open a browser. If the port is busy, an earlier `appa ui` still serves this page:
give its URL and do not start another. Stop the command when the user says
they are done, or leave it running if they move on to other work.
