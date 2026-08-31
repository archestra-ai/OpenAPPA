# Complete battery composition example

This is a self-contained example of matcher rows, complete-configuration
includes, and per-annotator `command` bindings.

The example is one deployment that includes the two batteries shipped in
the repository's top-level `batteries/` directory:

```text
examples/claude-code-battery/
├── appa.toml
└── local/
    └── read-sensitivity.py

batteries/
├── claude-code/
│   ├── appa.toml
│   └── read-sensitivity.py
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
replaces the shipped `Read` default with `local.read-sensitivity`, implemented
by the script in `local/`.
The local annotator keeps `.env.example` Public, restricts other dot-prefixed
paths, and additionally restricts `clients/`.

This command-based example requires a Unix system.

The Claude Code battery sends every Bash command to the Claude Code model
builtin. The model annotates the command before dispatch: its output label and
its required trust and audience.

The `Read` rule invokes `read-sensitivity.py` for one call. OpenAPPA writes
one JSON consult to standard input, reads one JSON answer from standard
output, and waits for the command to exit. An annotator that maps no `inputs`
receives the complete tool call in `args` — `name`, `description` when the
tool declares one, and `arguments`. Its answer is the call's complete
contract: `delta`, `requires`, and `emits`.

The Bash annotator controls declared information flows. It is not a shell or
network sandbox. A Claude Code deployment must use an OS sandbox to deny
network access and protect credentials and OpenAPPA files. Network ingress
should use `WebFetch` instead of Bash `curl`.

The Claude Code battery also labels `Read` results through
`read-sensitivity.py`. Hidden paths, credential and private-key names, system
secret locations, and sensitive symlink targets are private. Other paths are
Public.

The Slack battery needs no script. Every message needs trusted data and fresh
`hitl` attention. A deployment lets a channel through by adding a rule for it
in the root config. Channel-history results are private.

Run the Read annotator directly:

```sh
cd examples/claude-code-battery
printf '%s\n' '{"version":1,"kind":"annotation","name":"claude-code.read-sensitivity","declaration":{"inputs":[],"trust_ranks":["suspicious","trusted"],"audiences":["private"],"attention_marks":["hitl"],"effects":[]},"artifact":{"args":{"name":"Read","description":"Reads a file and returns its contents.","arguments":{"file_path":".env"}}}}' \
  | python3 ../../batteries/claude-code/read-sensitivity.py
```

The result restricts `.env` to `private`. Replace `.env` with
`README.md` to get a Public audience.

Run the local replacement annotator directly:

```sh
printf '%s\n' '{"version":1,"kind":"annotation","name":"local.read-sensitivity","declaration":{"inputs":[],"trust_ranks":["suspicious","trusted"],"audiences":["private"],"attention_marks":["hitl"],"effects":[]},"artifact":{"args":{"name":"Read","description":"Reads a file and returns its contents.","arguments":{"file_path":"clients/acme.txt"}}}}' \
  | python3 ./local/read-sensitivity.py
```

The result restricts `clients/acme.txt`. `.env.example` is the explicit local
exception and returns a Public audience.
