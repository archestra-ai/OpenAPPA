# Grain battery

Rules for the Grain MCP server (meeting recordings, notes, transcripts,
deals, and workspace settings). Add it to your root config with
`include`. The root config must define an authority named `hitl`; see
`examples/claude-code-battery/appa.toml`.

## Files

**`appa.toml`** — the rules, in five groups.

*Meeting content* — meetings, transcripts, notes, action items,
coaching feedback, clips, collections, stories, companies, people,
dossiers, and deals. The result may only reach readers allowed to see
Grain data (`internal`). Anything built from it cannot be sent to a
public place.

*Directory and settings* — workspace users, teams, smart topics, seat
counts, and settings. Internal, like the meetings.

*Links* — `resolve_urls` turns ids into URLs. No restriction.

*Writes inside the workspace* — creating clips, stories, collections,
smart topics, adding to them, tagging meetings. Trusted data; no
approval step.

*Sharing outward and administration* — making a collection visible to
anyone with the link, inviting people, assigning or removing paid
seats, moving people between teams, changing user, team, or workspace
settings. Each one needs a person to approve.

## Change the behaviour

To change a rule, add one for the same tool in your root config; root
rules run first. Nothing in this file needs editing.
