# OpenCode battery

This battery covers selected OpenCode V2 built-in tools. It does not cover every
tool or provide filesystem confinement. Install AppenCode in a host-controlled
directory. Keep its policy outside the agent's writable project.

Reads whose resolved path matches `*/private/*` narrow the trajectory audience
to `internal`. Other reads keep the current Label. Web tools require `public`
audience and return `suspicious` data. Edits and writes require `trusted` data.
The shell permits only `pwd`, while the audience remains `public` and trust
remains `trusted`. Other commands require explicit root contracts.

The deployment must enable `policy.deployment.context_control` for
`appa_subagent`. AppenCode gates child returns independently of this tool rule.
Code Mode, native subagents, and background shell requests remain refused by
the plugin. This battery does not enable them.

The battery grants no policy-management capability. An operator launches the
separate AppenCode setup session to configure a deployment. Its configuration
tool is absent from ordinary coding sessions.
