# OpenAPPA

OpenAPPA is the new home of the project previously developed as **baton**
inside `archestra-ai/archestra` (`ai-labs/baton`). The code will be migrated
here from archestra; until then this repo intentionally contains only this
README, `CLAUDE.md`, and `website/`.

## File Migration Proposal

Instructions for the agent migrating `ai-labs/baton` out of
`archestra-ai/archestra` into this repo. This exact migration was already
performed once (see git history up to commit `6fbe501`, later removed again
to hand the migration over cleanly), so every step below is verified.

### 1. Directory mapping

Take the full `ai-labs/baton` tree from current archestra `main` and place it
at the repo root with these renames (strip the `baton-` prefix; crate names
inside `Cargo.toml` files stay `baton-*` — do not rename crates):

| archestra `ai-labs/baton/...` | OpenAPPA |
|---|---|
| `baton-core/` | `core/` |
| `baton-check/` | `check/` |
| `baton-contracts/` | `contracts/` |
| `baton-dojo/` | `dojo/` |
| `baton-proxy/` | `proxy/` |
| `agentdojo-harness/` | `harness-agentdojo/` |
| `baton-demo/` | `demo/gateway/` |
| `demo/kagent/` | `demo/kagent/` |
| `docs/` | `docs/` |
| `baton-authority-model-design.md` | `docs/authority-model-design.md` |
| `baton-declassifier-design.md` | `docs/declassifier-design.md` |
| `README.md` | `README.md` (retitle to OpenAPPA, note the former baton name) |

### 2. Root files to create

- `Cargo.toml` — workspace replacing the crates' membership in the ai-labs
  workspace:

  ```toml
  [workspace]
  members = ["core", "check", "contracts", "dojo", "proxy"]
  resolver = "2"

  [workspace.lints.rust]
  dead_code = "deny"
  ```

- `rustfmt.toml` — copy `ai-labs/rustfmt.toml` verbatim (edition 2024,
  max_width 120; the crates are formatted against it).
- `.gitignore` — `target/` and `.env`.
- One `Cargo.lock` at the root (generate; `cargo check --workspace` does it).
  `demo/gateway` and `demo/kagent/notify-mcp` are standalone on purpose: they
  keep their empty `[workspace]` tables and their own `Cargo.lock` files.

### 3. Reference fixes (all verified necessary)

Cargo path deps:
- `check/`, `contracts/`, `dojo/`: `baton-core = { path = "../baton-core" }` → `"../core"`.
- `proxy/`: `"../baton-core"` → `"../core"`, `"../baton-contracts"` → `"../contracts"`.
- `demo/gateway/`: `"../baton-core"` → `"../../core"` (one level deeper now).

Rust sources (`.env` used to live at `ai-labs/.env`; now it is the repo root):
- `dojo/src/main.rs`: `join("../../.env")` → `join("../.env")`; update the
  `ai-labs/.env` comments/messages.
- `demo/gateway/src/bin/{gateway_agent,demo_agent}.rs`, `src/demo_support.rs`:
  the `join("../../.env")` path is still correct from `demo/gateway` (two
  levels up = repo root) — only update `ai-labs/.env` comment/message text.

Python (`harness-agentdojo/src/baton_dojo/`):
- `bridge.py`: `BATON_CHECK_DIR = Path(__file__).resolve().parents[3] / "baton-check"`
  and `AI_LABS_TARGET_DIR = BATON_CHECK_DIR.parents[1] / "target"` →

  ```python
  _REPO_ROOT = Path(__file__).resolve().parents[3]
  BATON_CHECK_DIR = _REPO_ROOT / "check"
  REPO_TARGET_DIR = _REPO_ROOT / "target"
  ```

  (rename all `AI_LABS_TARGET_DIR` uses; the binary lands in the root
  workspace `target/`).
- `pipeline.py`: `AI_LABS_ENV = ...parents[4] / ".env"` → `REPO_ENV = ...parents[3] / ".env"`;
  rename uses, including `tests/test_bench.py`'s `monkeypatch.setattr(pipeline, "AI_LABS_ENV", ...)`.

Shell scripts:
- `demo/kagent/run-demo.sh`: `.env` sourcing `../../../.env` → `../../.env`;
  docker build `(cd ../../.. && docker build -f baton/baton-proxy/Dockerfile ...)`
  → `(cd ../.. && docker build -f proxy/Dockerfile ...)`; message text.
- `demo/gateway/run-gateway-demo.sh` and `run-approver-demo.sh`: the
  `$CRATE_DIR/../../.env` lookup is still correct; the worktree fallback
  `$main_root/ai-labs/.env` → `$main_root/.env`; comment text.

Docker:
- `proxy/Dockerfile`: build context becomes the repo root
  (`docker build -f proxy/Dockerfile -t baton-proxy:poc .`); update the header comment.
- `proxy/Dockerfile.dockerignore`: rewrite for the new context — allow only
  `Cargo.toml`, `Cargo.lock`, `core/**`, `check/**`, `contracts/**`,
  `dojo/**`, `proxy/**`; keep the `**/target` and `proxy/wire-logs` excludes;
  drop the ai-labs crates (`runner`, `analyzer`, `cli`, `dashboard`) and the
  stale `baton-gateway` entry.

Docs/text (old path → new path, throughout):
- `../baton-core` → `../core`, `../baton-check` → `../check`, etc. in
  `harness-agentdojo/README.md`, `proxy/README.md`, `dojo/README.md`.
- `ai-labs/baton/baton-demo/run-*.sh` → `demo/gateway/run-*.sh` in
  `demo/gateway/README.md` and `APPROVER.md`; `cd ai-labs/baton/baton-proxy` → `cd proxy`.
- `baton-authority-model-design.md`/`baton-declassifier-design.md` links →
  `docs/...` (root README, `core/CLAUDE.md`) or sibling-relative (within `docs/`).
- `baton-core/src/lib.rs` → `core/src/lib.rs`; `ai-labs/.env` → repo-root `.env`.
- `harness-agentdojo/README.md`: `export BATON_CHECK_BIN="$PWD/../../target/..."`
  → `"$PWD/../target/..."`; `( cd ../baton-check ... )` → `( cd ../check ... )`.

### 4. CI (`.github/`)

- `.github/workflows/ci.yml` — port of archestra's `ai-labs-rust-checks` job
  (from `.github/workflows/on-pull-requests.yml`) with a Python job added:
  - `rust-checks` on `ubuntu-latest` (Blacksmith runners are not set up here):
    checkout (pin by SHA, `persist-credentials: false`), `rustup show`, then
    `cargo fmt --all --check`, `cargo check --workspace --locked`,
    `cargo clippy --workspace --all-targets --locked -- -D warnings`,
    `cargo test --workspace --locked`.
  - `harness-tests`: `astral-sh/setup-uv` (pin by SHA), `uv sync --locked`,
    `uv run pytest`, with `working-directory: ./harness-agentdojo`.
  - Triggers: `push` to `main` + `pull_request`; `permissions: contents: read`.
- `.github/dependabot.yml`: `cargo` at `/`, `uv` at `/harness-agentdojo`,
  `github-actions` at `/`, weekly.
- **Org Zizmor policy (auditor persona) will fail the required check unless**:
  every dependabot update entry has `cooldown: { default-days: 7 }`, and the
  workflow has a `concurrency` block
  (`group: "ci-${{ github.event.pull_request.number || github.ref }}"`,
  `cancel-in-progress: true`). Pin all action versions by commit SHA.

### 5. Repo rules

- Direct pushes to `main` are rejected: the org-required workflow
  "Zizmor GitHub Actions Static Analysis" only runs on PRs, so everything
  lands via PR.

### 6. Archestra-side cleanup (in archestra, after the copy)

- Delete `ai-labs/baton/`.
- Remove the five `baton/*` entries from `ai-labs/Cargo.toml`
  `[workspace] members` and refresh `ai-labs/Cargo.lock`
  (`cargo metadata` from `ai-labs/` is enough); verify with
  `cargo check --workspace --locked`.
- The `ai-labs-rust-checks` CI job and the dependabot `cargo`/`ai-labs`
  entry stay (other ai-labs crates use them).

### 7. Verification (expected results from the previous run of this migration)

- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`: clean.
- `cargo test --workspace --locked`: all pass (222 tests at archestra commit `f36b23c2d`).
- `cargo check --locked` in `demo/gateway` (its tests pass too) and `demo/kagent/notify-mcp`.
- `cd harness-agentdojo && uv sync --locked && uv run pytest`: all pass (26 tests).
- Repo-wide grep for `ai-labs`, `baton/baton`, `../baton-` returns nothing.
