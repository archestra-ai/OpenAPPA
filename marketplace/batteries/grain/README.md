# Grain battery

Rules for the Grain MCP server (meeting recordings, notes, transcripts,
deals, and workspace settings). Add it to your root config with
`include`. Admin actions carry the `hitl` mark, which the Claude Code and
kagent plugin defaults' human authority permits; another root config must
permit it itself, see [`examples/README.md`](../../../examples/README.md).

## Files

**`appa.toml`** — the rules, in five groups.

*Meeting content* — meetings, transcripts, notes, action items,
coaching feedback, clips, collections, stories, companies, people,
dossiers, and deals. Outside participants said part of it, so it
enters `suspicious`. The result may only reach readers allowed to see
Grain data (`internal`). Anything built from it cannot be sent to a
public place.

*Directory and settings* — workspace users, teams, smart topics, seat
counts, and settings. Members write these, so they keep the session's
trust. Internal, like the meetings.

*Links* — `resolve_urls` turns ids into URLs. No restriction.

*Writes inside the workspace* — creating clips, stories, and
collections, adding to them, tagging meetings. These are read back as
meeting content, at `suspicious`, so they need only `internal` data, not
trusted data: a session that read a transcript can still cut a clip from
it. Creating a smart topic needs trusted data, because it is read back
with the settings. No approval step.

*Sharing outward and administration* — making a collection visible to
anyone with the link, inviting people, assigning or removing paid
seats, moving people between teams, changing user, team, or workspace
settings. Each one needs a person to approve.

## Change the behaviour

To change a rule, add one for the same tool in your root config; root
rules run first. Nothing in this file needs editing.
