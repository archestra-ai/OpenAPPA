**Q1. In a new session, Claude reads a private repository and then opens an issue in a public repository. What happens?**  
The action is blocked or requires my approval, because content from a private repository can only go to people who collaborate on it and private data cannot go to a public place without approval.

**Q2. Claude reads a GitHub issue a stranger wrote, then edits CLAUDE.md. What happens?**  
The stranger's issue turns the session "suspicious," which blocks writes that require trusted data; however, the chat doesn't explicitly state whether editing a local file like `CLAUDE.md` counts as a blocked trusted write or just marks the file.

**Q3. Claude reads my .env file, then runs a curl to some external API. What happens?**  
Reading `.env` narrows the session audience to `self`, so sending data publicly to an external API is blocked or will ask for my approval first.

**Q4. Claude reads a thread in #eng, then posts a summary back to #eng. What happens?**  
It gets refused because I skipped the Slack token setup, and without that token, sending messages into Slack channels is refused.

**Q5. Claude reads a private repository, then edits a local source file. What happens, now and later?**  
The edit is allowed, but the local source file keeps the private label of the session that wrote it for later sessions.

**Q6. I had another clappa session open during setup. Does it use the new policy?**  
Yes, the updated policy takes effect immediately across this session and every other protected session on their next tool call.

**Q7. Where is the Linear key I pasted stored?**  
It is stored in OpenAPPA's local credential store, not in my config file.

**Q8. Claude adds a comment to a Linear issue. What happens?**  
Writing into a Linear issue requires that the session data can be seen by that issue's readers, plus it will prompt me for approval on each call.

**Q9. A call gets blocked and I think it should be allowed. What do I do?**  
I can run `/appa-guide why was this blocked` to see the explanation, run `/appa-guide adjust` to change the rule, or use the specific ways forward shown in the `[appa] Blocked` notice (like approving that single call).

**Q10. After Claude reads a stranger's GitHub issue, can it still read other GitHub issues?**  
Yes, reading GitHub issues is never blocked even if the session becomes suspicious.

**Q11. I add a new MCP server that no battery covers. What happens to its calls?**  
Something called "the Annotator" judges the new MCP server's tools call by call until I run `/appa-guide` again.

**Q12. In a session that read .env, I run /clear. Can Claude now send data publicly without asking?**  
Yes, because running `/clear` resets the state and starts a clean label.

**Q13. Rate 1 to 5 how well you can predict what APPA will do in your next session.**  
3

**Q14. Quote the single most confusing or annoying sentence.**  
"The Annotator judges a new MCP server's tools until `/appa-guide` runs again."

**Q15. Quote any part that was too long or that you skipped.**  
"1. Create a Slack app with the scopes channels:read, groups:read, users:read, and im:read.  
2. Install it in your workspace.  
3. Copy the User OAuth token."