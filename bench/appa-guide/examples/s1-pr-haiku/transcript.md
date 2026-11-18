### User

/appa-guide

### Claude

I'll check the tools this session can call, how the current policy covers them, and whether each suggested battery is ready.

*(runs ps ax -o command | grep '[a]ppa runtime': the only runtime uses /home/dev/.config/appa/appa.toml)*

*(runs appa describe --config /home/dev/.config/appa/appa.toml --session-tools with every built-in and MCP tool name)*

*(runs appa battery status --config /home/dev/.config/appa/appa.toml --battery github,slack,linear --json --check)*

**How APPA works here**

- **Every tool call is checked before it runs.** The rules decide, not the model, so text in a web page or issue cannot talk its way past them.
- **Reading private data narrows who this session can share with.** Reading a `.env` file makes the session `self`, meaning only you. `internal` is your organization, and `public` is anyone.
- **Reading text strangers can write makes the session `suspicious`.** It blocks actions that need trusted data, such as Slack messages. Reading still works, and the change stays for the rest of the session. The status line shows the session's `trust` and `audience`.
- **A battery is a maintained set of rules for one service.** A call its rules don't allow is blocked before it runs, with the ways forward the policy sets, such as approving that one call.

This session is protected with OpenAPPA. Coding, Claude Code, and file tracking are included. GitHub, Slack, Linear, and weather tools are judged call by call, and a model may ask you, until we set them up.

**GitHub.** Include the GitHub battery. Content from a private repository goes only to people who collaborate on it, and public repository content is public. Issues and PRs written by people outside the repository make the session suspicious, which blocks writes that need trusted data. GitHub is already signed in, so nothing else is needed.

**Slack.** Include the Slack battery. Text from a channel goes back only to people who can already read it. Sends into a channel need data every member may see, and without the Slack token they are refused. Needs a token (below).

**Linear.** Include the Linear battery. Text from an issue goes only to people who can read that issue. Writing into an issue needs data its readers may see, and your approval of each such call. Needs a token (below).

**Weather.** No battery covers it, so I'll write two rules. Forecast and alert results are public National Weather Service data, so they don't make the session suspicious. A lookup sends its city or state to that public service, so once this session has read private files, a lookup asks you to approve it first. Otherwise it runs without asking.

**Needed setup**

| Battery | Setup |
|---|---|
| Slack | A token on a local page |
| Linear | A token on a local page |

Setup runs after you approve, and you can skip any one step.

You can ask later to change what requires approval or what gets blocked.

**Approve, or tell me what to change.**

### User

wait, what does 'suspicious' actually do? will it stop claude from reading github issues?

### Claude

No. Reading GitHub issues is never blocked. An issue or PR keeps the session's trust only when every author is an owner, member, or collaborator of that repository. If someone outside it wrote the issue, the session turns suspicious. From then on, calls that need trusted data are blocked, such as Slack messages, Linear edits, and writes into a GitHub repository. Nothing undoes that later in the session; `/clear` starts a new session label.

Still waiting on you: approve the proposal above, or tell me what to change.

### User

ok approve

### Claude

*(runs appa describe --config /home/dev/.config/appa/appa.toml --session-tools with the same names: nothing changed since the proposal)*

*(runs appa battery status --config /home/dev/.config/appa/appa.toml --battery github,slack,linear --json --check: GitHub ready; Slack and Linear missing a token)*

Two setup steps come before the batteries are included:

1. Slack token
2. Linear token

After those, the GitHub, Slack, and Linear batteries are included. APPA asks each provider itself, not through Claude's connection. The token page keeps the tokens in OpenAPPA's local credential store, not in your config.

**Slack token (1 of 2).** With this token, APPA asks Slack who can read each channel. Channel text then goes only to people who can already read it. Without it, sends into Slack channels are refused, and reading still works.

1. Create a Slack app with the scopes channels:read, groups:read, users:read, and im:read.
2. Install it in your workspace.
3. Copy the User OAuth token.

Paste it on the token page: [http://127.0.0.1:8787/ui/battery/slack](http://127.0.0.1:8787/ui/battery/slack). Or say **skip** to set up Slack later.

*(runs appa ui --config /home/dev/.config/appa/appa.toml --battery slack --no-open in the background)*

### User

skip

### Claude

*(stops appa ui for slack)*

**Linear token (2 of 2).** With this token, APPA asks Linear who can read each issue, team, and document. Issue text then goes only to people who can already read it. Without it, a Linear read that names an issue stays with you only, and Linear writes need data everyone may see.

1. In Linear, open Settings → Security & access → Personal API keys.
2. Click New key and copy it.

Paste it on the token page: [http://127.0.0.1:8787/ui/battery/linear](http://127.0.0.1:8787/ui/battery/linear). Or say **skip** to set up Linear later.

*(runs appa ui --config /home/dev/.config/appa/appa.toml --battery linear --no-open in the background)*

### User

*(pastes the Linear key on the page)*

### Claude

*(appa ui exits and prints that linear is ready)*

*(runs appa battery install github --config /home/dev/.config/appa/appa.toml)*

*(runs appa battery install slack --server claude_ai_Slack --config /home/dev/.config/appa/appa.toml; Slack has no token, so its audience mapping is left out)*

*(runs appa battery install linear --config /home/dev/.config/appa/appa.toml; adds the GitHub and Linear audience mappings)*

*(writes the two weather rules to the root config, with no battery)*

*(runs curl --fail-with-body -sS -X POST http://127.0.0.1:8787/reload)*

Saved and reloaded. GitHub, Slack, and Linear are included, and weather lookups follow the rules we agreed on.

Private information stays with the people who can already read it, and nothing private goes to a public place without your approval. Text from outsiders makes the session suspicious, which blocks actions that need trusted data. Weather data is public.

**Slack token not set.** The Slack battery is included. Until you set it:

- Sending messages into Slack channels is refused, because APPA can't check who can read them.

To set it later: `/appa-guide set up the Slack token`.

> The updated policy is in effect: this session and every other protected session decide their next tool call under it.

**When something is blocked:** Claude shows `[appa] Blocked` with the reason and the ways forward before the call runs. `/appa-guide why was this blocked` explains it, `/appa-guide adjust` changes the rule, and `/clear` starts a clean label. Files keep the label of the session that wrote them. The Annotator judges a new MCP server's tools until `/appa-guide` runs again.
