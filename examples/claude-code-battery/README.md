# Complete battery composition example

This is a self-contained example of matcher rows, complete-configuration
includes, and per-annotator `command` bindings.

The example is one deployment that includes the two batteries shipped in
the repository's `marketplace/batteries/` directory:

```text
examples/claude-code-battery/
├── appa.toml
└── local/
    └── read-sensitivity.py

marketplace/batteries/
├── claude-code/
│   └── appa.toml
└── slack/
    └── appa.toml
```

The root `appa.toml` supplies deployment-wide settings and includes both
shipped battery configurations. Root matcher rows always run before included
files. Included files run in `include` order, and rows within each file retain
source order. This gives the root explicit precedence without depending on the
physical position of its `include` key. The root also defines the `hitl`
Authority used by the Slack battery and the local Bash override.

The root demonstrates two customizations. It bypasses the shipped Bash model
classifier for `cargo test` and requires fresh `hitl` attention. It also
replaces the shipped `host/claude-code/Read` rules with `local.read-sensitivity`,
implemented by the script in `local/`. Every rule names its tool by the
canonical tool id: `host/claude-code/<name>` for a Claude Code built-in,
`mcp/<server>/<tool>` for an MCP server's tool.
The local annotator asks a person before any dot-prefixed path other than
`.env.example` is read, and before anything under `clients/`.

This command-based example requires a Unix system.

The Claude Code battery sends every Bash command to the Claude Code model
builtin. The model annotates the command before dispatch: its output label and
its required trust and audience.

The local `host/claude-code/Read` rule invokes `read-sensitivity.py` for one call. OpenAPPA writes
one JSON consult to standard input, reads one JSON answer from standard
output, and waits for the command to exit. An annotator that maps no `inputs`
receives the complete tool call in `args` — `name`, `description` when the
tool declares one, and `arguments`. Its answer is the call's complete
contract: `delta`, `requires`, and `emits`.

The Bash annotator controls declared information flows. It is not a shell or
network sandbox. A Claude Code deployment must use an OS sandbox to deny
network access and protect credentials and OpenAPPA files. Network ingress
should use `host/claude-code/WebFetch` instead of Bash `curl`.

The Claude Code battery labels `host/claude-code/Read` results with static rules: hidden
paths, credential and private-key names, and system secret locations narrow
the session to `self`, the requester. Other paths keep its label. The root
rule here replaces those rules.

The Slack battery needs no script. Every message needs trusted data and fresh
`hitl` attention. A deployment lets a channel through by adding a rule for it
in the root config. Channel-history results are `internal`.

Run the local replacement annotator directly:

```sh
cd examples/claude-code-battery
printf '%s\n' '{"version":1,"kind":"annotation","name":"local.read-sensitivity","declaration":{"inputs":[],"trust_ranks":["suspicious","trusted"],"audiences":[],"attention_marks":["hitl"],"effects":[]},"artifact":{"args":{"name":"host/claude-code/Read","description":"Reads a file and returns its contents.","arguments":{"file_path":"clients/acme.txt"}}}}' \
  | python3 ./local/read-sensitivity.py
```

The result requires fresh `hitl` attention for `clients/acme.txt`.
`.env.example` is the explicit local exception and requires nothing.
