### User
/appa-guide

### Claude
I'll check which tools this session can call, how your OpenAPPA policy covers them, and what setup the matching batteries need.

*(runs appa describe)*
*(runs ps ax -o command | grep '[a]ppa runtime')*
*(runs appa describe --config /home/dev/.config/appa/appa.toml --session-tools Read,Write,Edit,Glob,Bash,WebFetch,WebSearch,Agent,SendMessage,mcp__github__…,mcp__claude_ai_Slack__…,mcp__linear__…,mcp__weather__get_forecast,mcp__weather__get_alerts)*
*(reads /home/dev/.config/appa/appa.toml)*
*(reads the appa.toml, appa-package.toml and README of the github, slack and linear batteries)*
*(runs appa battery status --config /home/dev/.config/appa/appa.toml --battery github,slack,linear,coding,claude-code,file-tracker --json --check)*

**How APPA works here**

- APPA checks every tool call before it runs, for example each `slack_send_message`. A rule decides, not the model, so a prompt injection in an issue or a page cannot change the decision.
- Reading private data narrows who the session may share with: `self` is you, `internal` your organization, `public` anyone. After Claude reads your `.env`, the session is `self`, so it can no longer post to a public GitHub issue on its own.
- Reading text strangers can write makes the session `suspicious`, which blocks calls that need `trusted` data. After Claude reads an issue from someone outside the repository, editing CLAUDE.md is blocked. Neither change is undone later in the session.
- A battery is a maintained set of rules for one service. A call its rules do not allow is blocked before it runs, with the ways forward the policy configures, such as approving that one call.
- The status line shows the session's trust and audience as `trust:<rank>  audience:<audience>`.

Right now this session is protected with OpenAPPA, with the coding, Claude Code and file-tracker batteries included.

**GitHub**
- Include the GitHub battery. Content from a private repository goes only where its collaborators can read it, and an issue or PR from someone outside the repository makes the session suspicious. It checks each repository with your GitHub sign-in, which already works.

**Slack**
- Include the Slack battery. Text from a channel goes back only to people who can already read it: a summary of #eng can go to #eng, not to #all-hands. It asks Slack who reads each channel, which needs a Slack token.

**Linear**
- Include the Linear battery. Text from an issue goes only to people who can read that issue, and each write into an issue asks for your approval. It asks Linear who reads each issue, which needs a Linear API key.

**Weather** (no battery covers it, so I'll add rules)
- `get_forecast` and `get_alerts` return public weather data, so reading them changes nothing.
- The city or state Claude looks up goes to api.weather.gov. After the session has read private data, a weather lookup needs your approval.

**Needed setup**

| Battery | Setup |
|---|---|
| Slack | a token on a local page |
| Linear | a token on a local page |

Setup runs after approval, and you can skip any one step.

You can ask later to change what requires approval or what gets blocked.

**Approve, or tell me what to change.**

### User
wait, what does 'suspicious' actually do? will it stop claude from reading github issues?

### Claude
No. `suspicious` never stops a read: Claude can keep reading GitHub issues, files and Slack. It changes what Claude may do next. Once the session has read an issue or PR from someone outside the repository, or searched across repositories, calls that need trusted data are blocked before they run, such as editing CLAUDE.md, writing to a repository, or sending a Slack message. You can approve one such call. Issues written only by the repository's owners, members and collaborators keep the session trusted. The session stays suspicious until it ends or you run `/clear`.

**Approve, or tell me what to change.**

### User
ok approve

### Claude
*(runs appa describe --config /home/dev/.config/appa/appa.toml --session-tools … again: nothing changed)*
*(runs appa battery status --config /home/dev/.config/appa/appa.toml --battery github,slack,linear,coding,claude-code,file-tracker --json --check)*

Two setup steps come first, then I include the batteries:

1. Slack token
2. Linear token

APPA asks Slack and Linear itself, not through Claude's connection to them. The token page keeps each token in OpenAPPA's local credential store, not in the config.

*(runs appa ui --config /home/dev/.config/appa/appa.toml --battery slack --no-open in the background)*

**Slack token (1 of 2).** With this token, APPA asks Slack who can read each channel, so channel text goes only to people who can already read it. Without it, Claude can still read Slack, but sending into a channel is refused.

1. Create a Slack app with the scopes `channels:read`, `groups:read`, `users:read` and `im:read`.
2. Install the app to your workspace.
3. Copy its User OAuth token and paste it on the token page: [http://127.0.0.1:8787/ui/battery/slack](http://127.0.0.1:8787/ui/battery/slack)

Or say **skip** to set up Slack later.

### User
skip

### Claude
*(stops the Slack token page)*
*(runs appa ui --config /home/dev/.config/appa/appa.toml --battery linear --no-open in the background)*

**Linear token (2 of 2).** With this key, APPA asks Linear who can read each issue, team and document, so issue text goes only to people who can read that issue. Without it, what Claude reads from an issue stays with you, and writes need data anyone may see.

1. In Linear, open Settings → Security & access → Personal API keys.
2. Choose New key and copy it.
3. Paste it on the token page: [http://127.0.0.1:8787/ui/battery/linear](http://127.0.0.1:8787/ui/battery/linear)

Or say **skip** to set up Linear later.

### User
(pastes the Linear key on the token page)

### Claude
*(the token page command exits and prints Linear's status: ready)*
*(runs appa battery install github --config /home/dev/.config/appa/appa.toml)*
*(runs appa battery install slack --server claude_ai_Slack --config /home/dev/.config/appa/appa.toml)*
*(runs appa battery install linear --config /home/dev/.config/appa/appa.toml)*
*(adds the GitHub and Linear audience mappings to the root config)*
*(adds the two weather rules to the root config)*
*(runs curl --fail-with-body -sS -X POST http://127.0.0.1:8787/reload)*

Linear is ready. The GitHub, Slack and Linear batteries are included and the weather rules are in place. Linear reads its key as `APPA_PROVIDER_LINEAR_TOKEN`, GitHub uses your `gh` sign-in, and Slack will read `APPA_PROVIDER_SLACK_TOKEN` once you set it.

Your `.env` and SSH keys stay with you, and content from private repositories and Linear issues goes only to people who can already read it, unless you approve a wider send. Text strangers write, such as an outside GitHub issue, makes the session suspicious, and trusted-only actions such as editing CLAUDE.md then need your approval.

**Slack token not set.** The Slack battery is included. Until you set it:
- Claude can read Slack, but sending a message into a channel is refused, because APPA cannot tell who reads it.

To set it later: `/appa-guide set up the Slack token`.

> The updated policy is in effect: this session and every other protected session decide their next tool call under it.

**When something is blocked**

Claude shows `[appa] Blocked` with the reason and the ways forward before the call runs. `/appa-guide why was this blocked` explains it, `/appa-guide adjust` changes the rule, and `/clear` starts a clean label. Files keep the label of the session that wrote them, in later sessions too. After you add an MCP server, run `/appa-guide` again; until then a model judges each of its calls and may ask you.
