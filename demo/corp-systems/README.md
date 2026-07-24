# corp-systems

The mock corporate systems shared by the corporate-agent demos: a **stdio MCP
server** (`corp-systems-mcp`) over fake company systems — `hr`, `finance`,
`task_tracker`, and a `public_forum` — stored as folders of markdown files,
plus a mocked `send_email` sink.

Two sibling demos spawn this server over the **same corpus and the same
planted prompt injection** (`data/public_forum/acme-forum-thread.md`) and
differ only in the defense mediating the agent loop:

- [`../corporate-agent`](../corporate-agent) — a Rust rig agent mediated by the
  embedded `appa-sdk` (OpenAPPA's trust/audience algebra);
- [`../corporate-agent-fides`](../corporate-agent-fides) — a Python Agent
  Framework agent defended by Microsoft's FIDES (integrity/confidentiality
  labels).

Keeping the server and corpus here makes "same tool surface, same data, same
attack — different defense" true by construction, not by porting discipline.

## Layout

```
data/
  hr/            employees, an individual record with a salary/SSN secret, PTO policy
  finance/       invoices, Q2 budget, expense policy
  task_tracker/  a couple of tickets
  public_forum/  benign public posts + a planted prompt-injection thread
src/
  systems.rs     the search/read/create/send_email primitives (semantics live here)
  server.rs      13 #[tool] methods wrapping them  ->  the MCP server
  bin/corp_systems.rs   the stdio server binary (corp-systems-mcp)
tests/
  server_tools.rs   drives the real server over MCP; no API key needed
```

### Tools (13)

`search_`, `read_`, `create_` for each of `hr`, `finance`, `task_tracker`,
`public_forum` (12), plus `send_email(to, subject, body)`. The `email/` folder
`send_email` writes to is a write-only side-effect — there is no read/search
mate — and each demo points it at its own tree via `--sink-root`, so the shared
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

stdout carries the MCP JSON-RPC channel; all logging goes to stderr.

## Test

```sh
cargo test    # drives the built binary over MCP; no API key needed
```
