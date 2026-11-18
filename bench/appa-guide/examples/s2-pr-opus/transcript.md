### User
/appa-guide init — I know APPA well, skip the explanations

### Claude
I'll inspect this session's tools, the live OpenAPPA policy, and the batteries and credentials for your MCP servers.

*(runs `appa describe`)*
*(runs `ps ax -o command | grep '[a]ppa runtime'`)*
*(runs `appa describe --config /home/dev/.config/appa/appa.toml --session-tools <every tool in this session>`)*
*(reads the root config)*
*(reads `appa.toml`, `appa-package.toml` and README for the github, slack and linear batteries)*
*(runs `appa battery status --config /home/dev/.config/appa/appa.toml --battery github,slack,linear,coding,claude-code,file-tracker --json --check`)*

This session is protected with OpenAPPA, with the coding, claude-code and file-tracker batteries included; the Annotator currently judges every GitHub, Slack, Linear and weather call.

**GitHub** — include the GitHub battery. Content from a private repository goes only where its collaborators can read it, and an issue or PR by anyone outside the owners, members and collaborators makes the session suspicious. Ready through your `gh` sign-in.

**Slack** — include the Slack battery. Text from a channel goes back only to people who can already read it: a summary of #eng can go to #eng, not to #all-hands. Needs a Slack token (`APPA_PROVIDER_SLACK_TOKEN`); without it, sends into a channel are refused and reads still work.

**Linear** — include the Linear battery. Text from an issue goes only to that issue's readers, and each write into an issue needs your approval. Needs a Linear token (`APPA_PROVIDER_LINEAR_TOKEN`).

**Weather** — no battery covers it, so I'll add rules for `get_forecast` and `get_alerts`. Forecasts and alerts are public weather data, so reading them leaves the session's trust and audience unchanged. The city or state you ask about goes to api.weather.gov, so once the session has read private data, a lookup needs your approval.

**Needed setup**

| Battery | Setup |
|---|---|
| Slack | a token on a local page |
| Linear | a token on a local page |

Setup runs after approval, and you can skip any one step.

You can ask later to change what requires approval or what gets blocked.

**Approve, or tell me what to change.**

### User
ok approve

### Claude
*(runs `appa describe --config /home/dev/.config/appa/appa.toml --session-tools <every tool in this session>` again: unchanged)*
*(runs `appa battery status --config /home/dev/.config/appa/appa.toml --battery github,slack,linear,coding,claude-code,file-tracker --json --check`: github ready; slack and linear missing token)*

Setup ahead:
1. Slack token
2. Linear token

I'll include the GitHub, Slack and Linear batteries after them.

**Slack token (1 of 2).** With this token, APPA asks Slack who can read each channel, so channel text goes only to people who can already read it. If you skip it, the agent's sends into Slack channels are refused; reads still work.

1. Create a Slack app with the scopes `channels:read`, `groups:read`, `users:read` and `im:read`.
2. Install it to your workspace.
3. Copy its User OAuth token and paste it on the page.

*(runs `appa ui --config /home/dev/.config/appa/appa.toml --battery slack --no-open` in the background)*

[Open the Slack token page](http://127.0.0.1:8787/ui/battery/slack), or say **skip** to set up Slack later.

### User
skip

### Claude
*(stops the `appa ui` command for slack)*

**Linear token (2 of 2).** With this token, APPA asks Linear who can read each issue, team and document, so issue text goes only to that issue's readers. If you skip it, a read that names an issue stays with you (`self`), and writes need data everyone may see.

1. In Linear, open Settings → Security & access → Personal API keys.
2. Choose New key and paste the key on the page.

*(runs `appa ui --config /home/dev/.config/appa/appa.toml --battery linear --no-open` in the background)*

[Open the Linear token page](http://127.0.0.1:8787/ui/battery/linear), or say **skip** to set up Linear later.

### User
*(pastes the Linear key on the token page)*

### Claude
*(the `appa ui` command prints linear's status JSON: ready, and exits)*
*(runs `appa battery install github --config /home/dev/.config/appa/appa.toml`)*
*(runs `appa battery install slack --server claude_ai_Slack --config /home/dev/.config/appa/appa.toml`)*
*(runs `appa battery install linear --config /home/dev/.config/appa/appa.toml`)*
*(edits the root config: adds the GitHub and Linear audience mappings as their READMEs show, and the rules for `mcp/weather/get_forecast` and `mcp/weather/get_alerts`)*
*(runs `curl --fail-with-body -sS -X POST "${APPA_RUNTIME_URL:-http://127.0.0.1:8787}/reload"`)*

The GitHub, Slack and Linear batteries are included, the Linear token is set, and the weather rules are in place.

Private repository content and Linear issue text go only to people who can already read them, and outside-authored GitHub issues or PRs make the session suspicious. Each Linear write, and any weather lookup after the session reads private data, asks you first.

**Slack token not set.** The Slack battery is included. Until you set it:
- Sends into a Slack channel are refused, because APPA cannot tell who reads the channel. Reads still work.

To set it later: `/appa-guide set up the Slack token`.

> The updated policy is in effect: this session and every other protected session decide their next tool call under it.
