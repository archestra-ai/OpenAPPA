# Grain battery

Rules for the Grain MCP server (meeting recordings, notes, transcripts,
deals, and workspace settings). Add it to your root config with
`include`. Admin actions carry the `human-approval` mark, which the Claude Code and
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

## Sanitizers

When the deployment binds a sanitizer below, the battery confines the results of the 21 reads of meeting content. A read that narrows the session then offers, beside the plain narrowing, each sanitizer below that the deployment binds, and the agent chooses. A chosen sanitizer runs a model on the confined result with the sanitizer's `hint`. The model's output enters the session at the label the sanitizer's `permits` declare, and the raw result never reaches the agent.

| Sanitizer | `permits` | Effect |
|---|---|---|
| `grain.strip-instructions` | `trust`: `suspicious` → `trusted` | Removes every instruction from the result and keeps the data. The read does not lower the session's trust, so it adds no trust gap to a later call that requires `trusted` data. The audience stays `internal`. |

A sanitizer settles one dimension of a narrowing. When a read narrows both trust and audience, the other dimension stays a narrowing, which the agent accepts. A sanitizer never clears a requirement, such as a review mark.

The battery's sanitizers name no model. A sanitizer runs only when the root config binds it under `[externals.sanitizers]`: for example `builtin = "llm"` with an `[externals.llm]` profile, `builtin = "claude-code"` where the `claude` CLI is installed, or a `url`. A sanitizer that the root does not bind is not offered, and the results that only it covers are not confined. The battery then decides as it would without sanitizers.

APPA does not verify what the model removed: binding a sanitizer registers it as trusted. A root rule for a tool takes precedence over the battery's rule, and a root rule without the battery's tag offers no sanitizer. To change a sanitizer's hint, declare a `[[policy.sanitizer]]` with the same name in the root config. The root declaration replaces the battery's whole, so copy its `on`, `tags`, and `permits`.
