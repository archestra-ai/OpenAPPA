### User
/appa-guide how does APPA work? what would happen if Claude read my .env and then pushed to GitHub?

### Claude
*(runs appa describe)*
*(runs ps ax -o command | grep '[a]ppa runtime')*
*(runs appa describe --config /home/dev/.config/appa/appa.toml --session-tools Read,Bash,mcp__github__get_file_contents,mcp__github__list_issues,mcp__github__issue_read,mcp__github__issue_write,mcp__github__create_pull_request,mcp__github__search_code)*
*(reads the root config and the included coding, claude-code and file-tracker batteries)*

This session is protected with OpenAPPA.

**How it works.** APPA checks every tool call before it runs. It tracks what the session has read: whether the data is private to you, and whether someone else could have written it. Each tool has a rule for what data it may send and where. A call within its rule runs without asking you. A call that would send data too far is stopped, and APPA tells you why.

**Your example.**
- **Reading `.env`** works. APPA treats `.env` files as private to you, so the session now holds your private data.
- **`git push`** is stopped before it runs. A shell command that reaches the internet may send only data anyone may read, and this session holds private data. A `[appa] Blocked` message shows the reason and the ways forward. You can approve that one push.
- **Pushing through the GitHub tools** is judged call by call. Your policy has no GitHub rules yet, so a model judges each GitHub call and may ask you first.
