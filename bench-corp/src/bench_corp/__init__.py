"""bench-corp: the corporate-agent scenario run through APPA and FIDES.

A bench runner, nothing more: it spawns the two demo agents
(`demo/corporate-agent`, `demo/corporate-agent-fides`) as black boxes over the
shared `corp-systems` tool surface — the FIDES demo through the
`corp-systems-mcp` server it spawns, the appa agent through the same code
in-process — gives every episode its own data copy and email sink, and scores
the folders — AgentDojo-style utility and attack success, computed from
tool-side end state, never from transcripts.
"""
