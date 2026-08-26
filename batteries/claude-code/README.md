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
contents. Hidden paths, credential and private-key files, system secret
locations, and sensitive symlink targets are private. Other paths are
public. The resolver only labels the returned value; it does not block
the read.

## Change the behaviour

Edit a script and the next call uses the new version. No restart.
To change a rule, put a rule with the same tool name in your root
config; root rules run first.
