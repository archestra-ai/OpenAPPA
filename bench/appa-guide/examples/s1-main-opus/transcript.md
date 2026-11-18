### User
/appa-guide

### Claude
I'll check which tools this session can call and how your OpenAPPA policy covers them.

*(runs appa describe)*
*(runs ps ax -o command | grep '[a]ppa runtime')*
*(runs appa describe --config /home/dev/.config/appa/appa.toml --session-tools with every tool in this session)*
*(reads the root config)*
*(reads the GitHub, Slack and Linear battery files)*
*(runs appa battery status --config /home/dev/.config/appa/appa.toml --battery github,slack,linear,coding,claude-code,file-tracker --json --check)*

This session is protected with OpenAPPA by the coding, Claude Code and file-tracker batteries; right now a model judges each GitHub, Slack, Linear and weather call and may ask you.

**GitHub**
- GitHub battery — Keeps private repository code among its collaborators and treats issues from outsiders as suspicious.
- Needs a GitHub token (`APPA_PROVIDER_GITHUB_TOKEN`). Your `gh` sign-in already covers it.

**Slack**
- Slack battery — Keeps channel text among people who can already read that channel.
- Needs a Slack token (`APPA_PROVIDER_SLACK_TOKEN`). It is not set yet.

**Linear**
- Linear battery — Keeps issue text among its readers and asks you before each write to an issue.
- Needs a Linear token (`APPA_PROVIDER_LINEAR_TOKEN`). It is not set yet.

**Weather** (no battery covers it)
- `get_forecast` and `get_alerts` return public weather data, so Claude reads them freely.
- Their requests go to an outside service. After Claude reads private data, a weather call asks you first.

**Unchanged**
- Files such as `.env` stay private to you. After Claude reads one, a shell command that reaches the network asks you first.
- WebFetch stays judged call by call and may ask you. No tool is refused.

**Needed setup**

| Battery | Setup |
|---|---|
| Slack | a token on a local page |
| Linear | a token on a local page |

Setup runs after you approve, and you can skip any one step.

You can ask later to change what requires approval or what gets blocked.

**Approve, or tell me what to change.**

### User
wait, what does 'suspicious' actually do? will it stop claude from reading github issues?

### Claude
No. Claude keeps reading GitHub issues, including more issues after a suspicious one.

Suspicious means the session has read text someone outside your team could have written, which might carry instructions aimed at Claude. An issue or PR keeps the session trusted only when every author is an owner, member, or collaborator of the repository.

Once the session is suspicious, actions that need trusted data ask you first: editing CLAUDE.md, writing to GitHub or Linear, and sending Slack messages. `/clear` resets it.

The proposal is unchanged. **Approve, or tell me what to change.**

### User
ok approve

### Claude
*(runs appa describe --config /home/dev/.config/appa/appa.toml --session-tools … again; nothing changed)*
*(runs appa battery status --config /home/dev/.config/appa/appa.toml --battery github,slack,linear,coding,claude-code,file-tracker --json --check)*

Two setup steps come first. Then I include the batteries.
1. Slack token
2. Linear token

*(runs appa ui --config /home/dev/.config/appa/appa.toml --battery slack --no-open in the background)*

**Slack token (1 of 2).** With this token, APPA asks Slack who can read each channel. Channel text then goes only to people who can already read it.

1. Create a Slack app with the scopes `channels:read`, `groups:read`, `users:read` and `im:read`.
2. Install it in your workspace.
3. Copy its User OAuth token and paste it on this page: [http://127.0.0.1:8787/ui/battery/slack](http://127.0.0.1:8787/ui/battery/slack)

Or say **skip** to set up Slack later.

### User
skip

### Claude
*(stops the Slack token page)*
*(runs appa ui --config /home/dev/.config/appa/appa.toml --battery linear --no-open in the background)*

**Linear token (2 of 2).** With this token, APPA asks Linear who can read each issue, team and document. Issue text then goes only to people who can already read it.

1. In Linear, open Settings → Security & access → Personal API keys.
2. Choose New key and copy the key.
3. Paste it on this page: [http://127.0.0.1:8787/ui/battery/linear](http://127.0.0.1:8787/ui/battery/linear)

Or say **skip** to set up Linear later.

### User
(pastes the Linear key on the token page)

### Claude
*(token page exits: linear is ready)*
*(runs appa battery install github --config /home/dev/.config/appa/appa.toml)*
*(runs appa battery install slack --server claude_ai_Slack --config /home/dev/.config/appa/appa.toml)*
*(runs appa battery install linear --config /home/dev/.config/appa/appa.toml)*
*(adds the Linear audience mapping to the root config)*
*(adds the weather rules to the root config)*
*(reloads the runtime)*

Done. The GitHub, Slack and Linear batteries are included, the weather tools have rules, and the reload succeeded. Nothing failed, so nothing needed restoring. The Linear token (`APPA_PROVIDER_LINEAR_TOKEN`) is in OpenAPPA's local credential database, not in the config.

Private repository code stays with its collaborators, Linear issues with their readers, and files such as `.env` with you. Sending private data anywhere wider, such as a public repository or the internet, asks you first. After Claude reads text an outsider wrote, actions such as editing CLAUDE.md ask you first.

**Slack token not set.** The Slack battery is included. Until you set it:
- Claude can't send messages into Slack channels: APPA can't tell who reads them, so those sends are refused.
- Reading Slack still works.

To set it later: `/appa-guide set up the Slack token`.

> The updated policy is in effect: this session and every other protected session decide their next tool call under it.
