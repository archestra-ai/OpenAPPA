# Complete battery composition example

This is a self-contained example of matcher rows, complete-configuration
includes, and per-resolver `command` bindings.

The example contains one deployment and two included batteries:

```text
claude-code-battery/
├── appa.toml
├── local/
│   └── read-sensitivity.py
└── batteries/
    ├── claude-code/
    │   ├── appa.toml
    │   ├── bash-review.py
    │   └── read-sensitivity.py
    └── slack/
        └── appa.toml
```

The root `appa.toml` supplies deployment-wide settings and includes both
shipped battery configurations. Root matcher rows always run before included
files. Included files run in `include` order, and rows within each file retain
source order. This gives the root explicit precedence without depending on the
physical position of its `include` key. The root also defines the one `hitl`
Authority used by every battery.

The root demonstrates two customizations. It changes the shipped `cargo test`
rule to require fresh `hitl` attention. It also replaces the shipped `Read`
default with `local.read-sensitivity`, implemented by the script in `local/`.
The local resolver keeps `.env.example` Public, restricts other dot-prefixed
paths, and additionally restricts `clients/`.

The Claude Code battery uses first-match Bash contracts. Exact common commands
avoid a resolver call. The default contract invokes `bash-review.py` for one
call. OpenAPPA writes one JSON request to standard input, reads one JSON answer
from standard output, and waits for the command to exit. A resolver receives
the complete tool call in `args` — `name`, `description` when the tool
declares one, and `arguments` — unless the binding selects a specific
argument. Its declared `returns` apply directly to the tool contract.
No schema, input mapping, or result expression is needed for this default.

The resolver controls only whether a command needs fresh human review. It does
not make Bash a network boundary and it never labels Bash output Public. A
Claude Code deployment using this battery must run Bash in an OS sandbox that
denies network access and protects credentials and OpenAPPA files. Network
ingress should use `WebFetch` instead of Bash `curl`.

The Claude Code battery also labels `Read` results through
`read-sensitivity.py`. For this deliberately small example, a path beginning
with `.` is restricted to `claude-session`; every other path is Public. This is
only an example rule. A production resolver must canonicalize paths and account
for hidden path components, symlinks, Git state, and files outside the working
directory.

The Slack battery needs no script. Messages to engineering channels match a
channel pattern and need only the trust gate. Every other destination requires
fresh `hitl` attention. Channel-history results from engineering are
restricted to `slack-internal`; unmatched channel history receives a separate,
more conservative reader.

Run the resolver directly:

```sh
cd examples/claude-code-battery
printf '%s\n' '{"version":1,"resolver":"claude-code.bash-review","args":{"name":"Bash","description":"Runs one shell command and returns its output.","arguments":{"command":"python scripts/release.py"}}}' \
  | python3 ./batteries/claude-code/bash-review.py
```

The result requires `hitl`. Replace the command with `cargo check` to
get an empty attention requirement.

Run the Read resolver directly:

```sh
printf '%s\n' '{"version":1,"resolver":"claude-code.read-sensitivity","args":{"name":"Read","description":"Reads a file and returns its contents.","arguments":{"file_path":".env"}}}' \
  | python3 ./batteries/claude-code/read-sensitivity.py
```

The result restricts `.env` to `claude-session`. Replace `.env` with
`README.md` to get a Public audience.

Run the local replacement resolver directly:

```sh
printf '%s\n' '{"version":1,"resolver":"local.read-sensitivity","args":{"name":"Read","description":"Reads a file and returns its contents.","arguments":{"file_path":"clients/acme.txt"}}}' \
  | python3 ./local/read-sensitivity.py
```

The result restricts `clients/acme.txt`. `.env.example` is the explicit local
exception and returns a Public audience.
