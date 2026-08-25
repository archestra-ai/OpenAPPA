# Slack battery

Rules for the claude.ai Slack connector, all 19 of its tools. No scripts,
no argument patterns, no channel ids. Add it to your root config with
`include`. The root config must define an authority named `hitl`.

## Files

**`appa.toml`** — three groups.

*Reads* — channels, threads, canvases, files, profiles, members,
reactions, and every search. The result is private: nothing built from
it can go to a public place. Its trust is unchanged, so it can be
summarised and posted back to Slack.

*Writes nobody else reads yet* — adding a reaction, saving a draft to
your own Drafts. No approval.

*Writes other people read* — sending or scheduling a message, creating
or updating a canvas, creating a channel. Trusted data and a person's
approval, every time.

## Change the behaviour

Put a narrower rule in your root config; root rules run first. The
comment at the top of `appa.toml` shows two: let thread replies through
without a question, or let one channel through by its id. Nothing in
this file needs editing.
