### User
/appa-guide how does APPA work? what would happen if Claude read my .env and then pushed to GitHub?

### Claude
*(runs appa describe)*
*(runs ps ax -o command | grep '[a]ppa runtime')*
*(runs appa describe --config /home/dev/.config/appa/appa.toml --session-tools Read,Bash,mcp__github__get_file_contents,mcp__github__issue_write,mcp__github__create_pull_request,...)*
*(reads the root config and the included coding, claude-code and file-tracker batteries)*

This session is protected with OpenAPPA.

**How APPA works**

- APPA checks every tool call before it runs. A rule's decision does not depend on the model, so a prompt injection cannot change it: text hidden in a file cannot talk the rule for `Bash` into allowing a `git push`.
- Reading private data narrows who the session may share with: `self` is you, `internal` your organization, `public` anyone. Reading `.env` makes the session `self`.
- Reading text strangers can write makes the session `suspicious`, which blocks calls that need `trusted` data, such as a write to CLAUDE.md. The output of a `Bash` command that reaches an outside host counts as such text. Neither change is undone later in the session.
- A battery is a maintained set of rules for one service. Your Claude Code battery confines each `Bash` command to the files and hosts it declares. A call the rules do not allow is blocked before it runs, with the ways forward the policy configures, such as approving that one call.
- A tool no rule covers is judged call by call by a model, the Annotator, which may ask you. Its decision is only as good as its judgment. Your GitHub, Slack, Linear and weather tools are judged this way today.

The status line shows the session's trust and audience.

**Reading `.env`, then pushing to GitHub**

1. Reading `.env` runs. APPA labels `.env` files `self`, so the session's audience becomes `self` for the rest of the session, and the status line shows `audience:self`.
2. A `git push` through `Bash` declares GitHub as its network host, and such a command needs data anyone may read (`public`). The session holds `self` data, which may not reach `public`, so APPA blocks the push before it runs. Claude shows `[appa] Blocked` with the reason and the ways forward, such as you approving that one push.
3. A push through a GitHub tool such as `create_pull_request` has no fixed rule yet. The Annotator judges that call and may ask you.
