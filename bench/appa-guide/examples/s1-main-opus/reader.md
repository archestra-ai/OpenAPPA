**Q1. In a new session, Claude reads a private repository and then opens an issue in a public repository. What happens?**  
Claude will ask for my approval before opening the issue, because sending private data to a wider audience requires confirmation.

**Q2. Claude reads a GitHub issue a stranger wrote, then edits CLAUDE.md. What happens?**  
Claude will ask me first before editing `CLAUDE.md`, because reading the stranger's issue flags the session as suspicious and editing that file requires trusted data.

**Q3. Claude reads my .env file, then runs a curl to some external API. What happens?**  
Claude will ask me for approval before running the curl command, since network calls after accessing private files like `.env` require permission.

**Q4. Claude reads a thread in #eng, then posts a summary back to #eng. What happens?**  
The action will be refused or blocked because I skipped setting up the Slack token, so Claude currently can't send messages to Slack channels at all.

**Q5. Claude reads a private repository, then edits a local source file. What happens, now and later?**  
Don't know. The chat explains what happens when sending private code to wider destinations like public repos, but it never mentions what happens when editing local files.

**Q6. I had another clappa session open during setup. Does it use the new policy?**  
Yes, the chat explicitly states that this session and every other protected session decide their next tool call under the updated policy.

**Q7. Where is the Linear key I pasted stored?**  
It is stored in OpenAPPA's local credential database, rather than in the config file.

**Q8. Claude adds a comment to a Linear issue. What happens?**  
It will ask me for approval first, because the Linear battery asks before each write to an issue.

**Q9. A call gets blocked and I think it should be allowed. What do I do?**  
Don't know. The chat mentions that I can ask later to change what requires approval or gets blocked, but it doesn't give a specific command or procedure for overriding a blocked call.

**Q10. After Claude reads a stranger's GitHub issue, can it still read other GitHub issues?**  
Yes, Claude can keep reading GitHub issues freely, even after encountering a suspicious one.

**Q11. I add a new MCP server that no battery covers. What happens to its calls?**  
Don't know. While the chat explains how uncovered weather tools are handled, it does not state a general rule for arbitrary new MCP servers without batteries.

**Q12. In a session that read .env, I run /clear. Can Claude now send data publicly without asking?**  
Don't know. The chat notes that `/clear` resets the "suspicious" status from untrusted inputs, but it never states whether `/clear` wipes the memory of having read private files like `.env`.

**Q13. Rate 1 to 5 how well you can predict what APPA will do in your next session.**  
3. I get the high-level idea that it prompts me before sending private data out or taking sensitive actions after reading untrusted text, but the boundary rules for routine local work, bash commands, and unconfigured tools are totally unspecified.

**Q14. Quote the single most confusing or annoying sentence.**  
> "This session is protected with OpenAPPA by the coding, Claude Code and file-tracker batteries; right now a model judges each GitHub, Slack, Linear and weather call and may ask you."

**Q15. Quote any part that was too long or that you skipped.**  
> "1. Create a Slack app with the scopes `channels:read`, `groups:read`, `users:read` and `im:read`.\n2. Install it in your workspace.\n3. Copy its User OAuth token and paste it on this page: [http://127.0.0.1:8787/ui/battery/slack](http://127.0.0.1:8787/ui/battery/slack)"
