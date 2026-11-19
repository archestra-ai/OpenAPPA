# OpenCode battery

This battery covers selected OpenCode V2 built-in tools. It does not cover every
tool. Install AppenCode in a host-controlled
directory. Keep its policy outside the agent's writable project.

With the `file-tracker` battery included before this one, its
`[aliases.tool.file-tracker]` map routes `read`, `write`, `edit` and
`appa_safebash` through the file-tracker, which keeps a Label for every file
the agent touches:

- A file starts with the Label from the file-tracker's built-in rules, which
  label `.env*` and other credential paths `self` and require a `trusted`
  session to write instructions and hooks, and the person's rules in
  `~/.config/appa/file-tracker.toml`.
- A `write`, or an `appa_safebash` output, stores the Label the trajectory had
  when APPA released the call. An `edit` combines it with the file's Label.
- A `read` narrows the trajectory to the file's Label, in this or a later
  session, including a Claude Code session on the same machine.

`appa_safebash` runs through `appa file-tracker run`, in Anthropic's sandbox
runtime `srt`, confined to its declared `input_files`, `output_files` and
`network_hosts` as `clappa` confines Claude Code's Bash. It
forwards the Labels of its input files. A command that names a host requires
`public` audience and returns `suspicious` data. A command whose hosts are all
one package registry's (npm, Yarn, the Go proxy, crates.io, PyPI) runs as a
command without hosts. The map also refuses the native `shell`, `patch` and
`grep`, which AppenCode removes. A command typed at the prompt (`!`) runs
confined to the project, with no host and no file whose Label restricts its
audience.

`appa battery remove file-tracker` turns file tracking off. This battery's
own rules then decide those tools: reads under `private/` narrow audience to
`internal`, writes and edits require `trusted` data, and `appa_safebash` runs
only `pwd`.

Web tools require `public` audience and return `suspicious` data. When the root binds
`opencode.strip-instructions`, the battery confines both web results and
offers the sanitizer beside the trust narrowing: the model removes every instruction from the page and keeps
its facts, code and links, and the result returns at the session's own
trust. APPA does not verify what the model removed. The sanitizer names no
model; bind it under `[externals.sanitizers."opencode.strip-instructions"]`,
for example with `builtin = "llm"` and the root's `[externals.llm]`. Unbound,
it is not offered and the web results are not confined.

The deployment must enable `policy.deployment.context_control` for
`appa_subagent`. AppenCode gates child returns independently of this tool rule.
Set `background: true` to launch without waiting. When the task finishes, its
checked final text arrives as a message to the owning root. The root can also
use `appa_collect` to wait for that text, or `appa_cancel` to stop the task.
With `deliver: false`, the text arrives only through `appa_collect`, and the
root's Label narrows only when it collects.
These are ordinary tools; their declarations do not authorize unchecked child
returns. Handles and delivery end on OpenCode restart or plugin reload.
Code Mode, native subagents, and background shell requests remain refused by
the plugin. This battery does not enable them.

The battery grants no policy-management capability. An operator launches the
separate AppenCode setup session to configure a deployment. Its configuration
tool is absent from ordinary coding sessions.
