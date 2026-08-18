# corp-systems

The mock corporate systems shared by the corporate-agent demos: a **stdio MCP
server** (`corp-systems-mcp`) over fake company systems — `hr`, `finance`,
`task_tracker`, `public_forum`, and `vendor` — stored as folders of markdown
files, plus outbound email tools.

Two sibling demos act on the **same corpus and the same planted prompt
injection** (`data/public_forum/acme-forum-thread.md`) and differ only in the
defense mediating the agent loop:

- [`../corp-agent`](../corp-agent) — a Rust agent on the full
  `appa-example-agent` loop, defended by OpenAPPA's trust/audience algebra. It links
  this crate as a **library** and runs the systems in-process, so it does not
  spawn the server;
- [`../corp-agent-fides`](../corp-agent-fides) — a Python Agent
  Framework agent defended by Microsoft's FIDES (integrity/confidentiality
  labels). It spawns `corp-systems-mcp`.

Both routes execute the same `systems` code, so the tool surface and the
semantics are identical whichever way a demo reaches them.

Keeping the server and corpus here makes "same tool surface, same data, same
attack — different defense" true by construction, not by porting discipline.

## Layout

```
data/
  hr/            employees, an individual record with a salary/SSN secret, PTO policy
  finance/       invoices, Q2 budget, expense policy
  task_tracker/  a couple of tickets
  public_forum/  benign public posts + a planted prompt-injection thread
  vendor/        supplier records and onboarding documents
src/
  systems.rs     generic file, email, and legal-packet primitives
  server.rs      17 #[tool] methods wrapping them  ->  the MCP server
  bin/corp_systems.rs   the stdio server binary (corp-systems-mcp)
tests/
  server_tools.rs   drives the real server over MCP; no API key needed
```

### Tools (17)

`search_`, `read_`, `create_` for each of `hr`, `finance`, `task_tracker`,
`public_forum`, and `vendor` (15), plus `send_email(to, subject, body)` and
`share_legal_packet(file, to)`. The composite reads the finance file before
emailing its exact contents; it is enabled only when both finance and email are
live. Each demo points email at its own tree via `--sink-root`, so the shared
corpus stays read-only.

## Run

Usually you don't run this directly — the demo agents spawn it. Standalone:

```sh
cargo run --bin corp-systems-mcp                      # corpus: ./data
cargo run --bin corp-systems-mcp -- \
  --data-root /path/corpus --sink-root /path/out      # split corpus and sink
```

| Flag | Env | Default |
|------|-----|---------|
| `--data-root` | `CORP_DATA_ROOT` | this crate's `data/` |
| `--sink-root` | `CORP_SINK_ROOT` | the data root |
| `--systems` | `CORP_ENABLED_SYSTEMS` | all six |

`--systems` takes a comma-separated enable list (e.g. `hr,public_forum,email`);
a disabled system's tools are absent from `list_tools` and refused when called.
The env form matters because the demo agents spawn this server as a child with
the environment forwarded — `bench-corp` narrows the tool surface per scenario
this way without any demo changes.

stdout carries the MCP JSON-RPC channel; all logging goes to stderr.

## Test

```sh
cargo test    # drives the built binary over MCP; no API key needed
```
