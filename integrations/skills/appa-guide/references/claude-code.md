# Claude Code

The appa plugin for Claude Code ships its own `appa-guide` skill,
installed together with the runtime. Do not duplicate its flow here.

- Tell the user to run `/appa-guide` with `init` or `adjust` in this
  session, and stop.
- If the command or the plugin's skill is missing, the installation is
  incomplete: tell the user to run `appa init claude-code` and stop.
