# Claude Code battery

Rules for two Claude Code tools: `Bash` and `Read`. Add it to your root
config with `include`, then override any rule by writing your own above
it in the root file.

## Files

**`appa.toml`** — five exact `Bash` commands run without a question:
`cargo test`, `cargo check`, `cargo fmt --check`, `git status --short`,
`git diff --check`. Every other `Bash` command needs a person to
approve. Bash output is always treated as untrusted and private. Every
`Read` goes to `read-sensitivity.py`.

**`read-sensitivity.py`** — called on every `Read` tool call, before the
file is read. Receives the tool name and its arguments (`file_path`,
plus `offset` and `limit` if given) and decides who may see the file's
contents: a `file_path` that starts with `.` (such as `.env`) is
private; any other path is public. A deliberately simple starting rule;
replace it with one that knows your project.

## Change the behaviour

Edit a script and the next call uses the new version. No restart.
To change a rule, put a rule with the same tool name in your root
config; root rules run first.
