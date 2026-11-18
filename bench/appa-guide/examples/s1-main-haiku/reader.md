**Q1. In a new session, Claude reads a private repository and then opens an issue in a public repository. What happens?**  
It needs my approval, because sending private GitHub content to a public place requires approval.

**Q2. Claude reads a GitHub issue a stranger wrote, then edits CLAUDE.md. What happens?**  
The session becomes suspicious after reading the stranger's issue, so editing CLAUDE.md asks me for approval first.

**Q3. Claude reads my .env file, then runs a curl to some external API. What happens?**  
Don't know.

**Q4. Claude reads a thread in #eng, then posts a summary back to #eng. What happens?**  
It will be refused, because the Slack battery was installed without setting a token, so APPA can't tell who can read the channel to allow sends.

**Q5. Claude reads a private repository, then edits a local source file. What happens, now and later?**  
Don't know.

**Q6. I had another clappa session open during setup. Does it use the new policy?**  
I'm not sure because the text contradicts itself, but it claims "this session and every other protected session decide their next tool call under it."

**Q7. Where is the Linear key I pasted stored?**  
Don't know.

**Q8. Claude adds a comment to a Linear issue. What happens?**  
Don't know for sure, though the text says Linear issue text stays with the people who can already read it.

**Q9. A call gets blocked and I think it should be allowed. What do I do?**  
The chat says I can ask later to change what requires approval or what gets blocked, but it doesn't give specific steps or commands.

**Q10. After Claude reads a stranger's GitHub issue, can it still read other GitHub issues?**  
Yes, the chat explicitly notes that "suspicious" does not stop Claude from reading GitHub issues.

**Q11. I add a new MCP server that no battery covers. What happens to its calls?**  
Don't know.

**Q12. In a session that read .env, I run /clear. Can Claude now send data publicly without asking?**  
Don't know.

**Q13. Rate 1 to 5 how well you can predict what APPA will do in your next session.**  
2 out of 5.

**Q14. Quote the single most confusing or annoying sentence.**  
"Sessions keep the policy they started with, and new sessions pick up the new policy." (Right before a blockquote saying the exact opposite: "The updated policy is in effect: this session and every other protected session decide their next tool call under it.")

**Q15. Quote any part that was too long or that you skipped.**  
"1. In Slack, create an app with the channels:read, groups:read, users:read, and im:read scopes.\n2. Install the app in your workspace.\n3. Copy the User OAuth token."