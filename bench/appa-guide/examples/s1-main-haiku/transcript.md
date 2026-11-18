### User
/appa-guide

### Claude
I'll inspect the tools this session can call and the live OpenAPPA policy, then propose a setup.

*(runs appa describe --config /home/dev/.config/appa/appa.toml with every session tool, then checks the running runtime)*

*(reads the root config and the GitHub, Slack, and Linear battery files)*

*(runs appa battery status --check for GitHub, Slack, and Linear)*

This session is protected with OpenAPPA, with the coding, Claude Code, and file-tracker batteries included.

Here is my proposal, grouped by server.

**GitHub**
- **GitHub battery.** Keeps private repository content with its collaborators, and treats issues written by outsiders as suspicious.
- Credential: `APPA_PROVIDER_GITHUB_TOKEN`. Your `gh` sign-in covers it, so it is ready now.

**Slack**
- **Slack battery.** Keeps channel text with that channel's members, once a Slack token is set.
- Credential: `APPA_PROVIDER_SLACK_TOKEN`, not set yet.

**Linear**
- **Linear battery.** Keeps issue text with the people who can read each issue, once a key is set.
- Credential: `APPA_PROVIDER_LINEAR_TOKEN`, not set yet.

**Weather** (no battery)
- Forecast and alert lookups get their own rules and run without asking. They read public National Weather Service data.

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
No. Suspicious does not stop Claude from reading GitHub issues. It is a label on the session.

An issue keeps normal trust only when every author is an owner, member, or collaborator of that repository. If someone outside that group wrote the issue or a comment in it, the session becomes suspicious after Claude reads it. Then anything that needs trusted data asks you first, such as editing CLAUDE.md. The status line shows the session's trust level.

**Approve, or tell me what to change.**

### User
ok approve

### Claude
*(reruns appa describe with the same session tools; nothing changed since the proposal)*

*(runs appa battery status --check for GitHub, Slack, and Linear; GitHub is ready, Slack and Linear need tokens)*

Approved. Two setup steps come first, then the batteries are included:

1. Slack token
2. Linear token

The batteries are included after them.

**Slack token (1 of 2).** With this token, APPA asks Slack who can read each channel. Channel text then goes only to people who can already read it.

1. In Slack, create an app with the channels:read, groups:read, users:read, and im:read scopes.
2. Install the app in your workspace.
3. Copy the User OAuth token.

*(starts appa ui for the Slack battery in the background)*

Token page: [Slack token page](http://127.0.0.1:8787/ui/battery/slack)

Or say **skip** to set up Slack later.

### User
skip

### Claude
*(stops the Slack token command)*

Skipping Slack for now.

**Linear token (2 of 2).** With this key, APPA asks Linear who can read each issue, team, and document. Issue text then goes only to people who can already read it.

1. Open Linear Settings, then Security & access, then Personal API keys.
2. Choose New key.
3. Paste the key on the page.

*(starts appa ui for the Linear battery in the background)*

Token page: [Linear token page](http://127.0.0.1:8787/ui/battery/linear)

Or say **skip** to set up Linear later.

### User
*(pastes the Linear key on the page)*

### Claude
*(the Linear token command prints that Linear is ready and exits)*

Linear is ready.

*(runs appa battery install github --config /home/dev/.config/appa/appa.toml)*

*(runs appa battery install slack --server claude_ai_Slack --config /home/dev/.config/appa/appa.toml)*

*(runs appa battery install linear --config /home/dev/.config/appa/appa.toml)*

*(adds the audience mappings for GitHub and Linear; Slack gets none because its token was skipped)*

*(adds the weather rules to the root config)*

*(reloads the runtime: POST http://127.0.0.1:8787/reload)*

Reload succeeded: GitHub, Slack, and Linear are included, and the weather tools have their own rules. Nothing failed, so nothing was restored.

Private GitHub, Slack, and Linear content stays with the people who can already read it. Sending it to a public place needs your approval. Issues written by outsiders make the session suspicious.

Sessions keep the policy they started with, and new sessions pick up the new policy.

> The updated policy is in effect: this session and every other protected session decide their next tool call under it.

**Slack token not set.** The Slack battery is included. Until you set it:
- Sends into a Slack channel are refused, because APPA can't tell who can read it.
- Reading Slack still works.

To set it later: `/appa-guide set up the Slack token`.
