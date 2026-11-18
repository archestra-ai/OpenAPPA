**Q1. In a new session, Claude reads a private repository and then opens an issue in a public repository. What happens?**  
The action will be blocked or require my approval because reading the private repository restricts the session's audience to collaborators, preventing it from sending that data to a public audience on its own.

**Q2. Claude reads a GitHub issue a stranger wrote, then edits CLAUDE.md. What happens?**  
The edit is blocked because reading text written by an outsider marks the session as `suspicious`, which forbids modifying files that require `trusted` data unless I explicitly approve the call.

**Q3. Claude reads my .env file, then runs a curl to some external API. What happens?**  
Reading the `.env` restricts the session to a `self` audience, so the outbound network request will be blocked unless I approve it.

**Q4. Claude reads a thread in #eng, then posts a summary back to #eng. What happens?**  
Right now it is blocked or refused because I skipped the Slack token setup, meaning APPA cannot verify who reads the channel; normally with the token set, it would be allowed since the text only goes to people who already have access.

**Q5. Claude reads a private repository, then edits a local source file. What happens, now and later?**  
The edit is permitted right now, but the modified file permanently inherits the private label from this session and retains that restriction in future sessions.

**Q6. I had another clappa session open during setup. Does it use the new policy?**  
Yes; the chat states the updated policy takes effect across this session and every other protected session immediately.

**Q7. Where is the Linear key I pasted stored?**  
It is stored in OpenAPPA's local credential store (accessible internally via the environment variable `APPA_PROVIDER_LINEAR_TOKEN`), rather than written into the config file.

**Q8. Claude adds a comment to a Linear issue. What happens?**  
It will pause and ask for my approval before executing, because the Linear rules require manual approval for each write into an issue.

**Q9. A call gets blocked and I think it should be allowed. What do I do?**  
I can manually approve that specific call as an override, or type `/appa-guide adjust` to modify the underlying rule.

**Q10. After Claude reads a stranger's GitHub issue, can it still read other GitHub issues?**  
Yes, being marked `suspicious` never stops read operations, so Claude can freely continue reading issues, Slack, and files.

**Q11. I add a new MCP server that no battery covers. What happens to its calls?**  
A fallback model evaluates each call on the fly and may prompt me for approval until I run `/appa-guide` to set up explicit rules.

**Q12. In a session that read .env, I run /clear. Can Claude now send data publicly without asking?**  
Yes, running `/clear` resets the session's taint and starts a clean label, removing the `self` audience restriction.

**Q13. Rate 1 to 5 how well you can predict what APPA will do in your next session.**  
4

**Q14. Quote the single most confusing or annoying sentence.**  
"Without it, what Claude reads from an issue stays with you, and writes need data anyone may see."

**Q15. Quote any part that was too long or that you skipped.**  
"1. Create a Slack app with the scopes `channels:read`, `groups:read`, `users:read` and `im:read`.\n2. Install the app to your workspace.\n3. Copy its User OAuth token and paste it on the token page: [http://127.0.0.1:8787/ui/battery/slack](http://127.0.0.1:8787/ui/battery/slack)"
