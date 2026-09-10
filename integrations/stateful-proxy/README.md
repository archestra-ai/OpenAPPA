# Experimental Stateful Proxy

This directory packages an experimental, standard-library Python proof of concept for an HTTP/SSE-only stateful provider relay. It keeps provider tool-call identifiers stable across relay hops, records local lifecycle facts in a private SQLite ledger, and can fail closed while delegating decisions to an OpenAPPA runtime.

It is not a supported OpenAPPA integration, production deployment, provider adapter, or universal authorization proof. The relay consumes raw client identity assertions and provider protocol shapes. Those assertions are not authenticated identity, and a tool-call/result join does not prove execution or authorization.

## Contents

- `src/stateful_proxy/proxy.py`: HTTP/SSE relay, tool-call rewrite, lifecycle mediator, and secure key-file loading.
- `src/stateful_proxy/lifecycle_ledger.py`: private durable lifecycle bindings.
- `src/stateful_proxy/provider_history.py`: canonical provider-history comparison.
- `src/stateful_proxy/rewritten_arguments.py`: documented spawn-argument rewrites only.
- `src/stateful_proxy/checkpoint_client.py`: detached checkpoint client.
- `src/stateful_proxy/appa_*_gate.py`: runtime and review Gate facades.
- `src/stateful_proxy/strict_broker.py`: loopback authenticated-review broker with durable audit-before-grant behavior.
- `src/stateful_proxy/review_client.py` and `review_relay.py`: authenticated reviewer and relay helpers.
- `src/stateful_proxy/fixture_tools.py` and `fixture_runner.py`: synthetic fixture tools and an explicitly configured runner.
- `fixtures/golden-scenarios.json`: scenario requirements, not captured client traffic.

## Bounded Live Results

Private live validation exercised only HTTP/SSE traffic and synthetic private/public fixtures. The evidence remains private. The observed client, version, and model combinations were Claude Code `2.1.258` with Haiku 4.5, Codex `0.153.0` with GPT-5.4, and OpenCode `1.18.29` with Kimi for Coding.

| Observed case | Bounded live pass | Limit |
| --- | --- | --- |
| Fork and compaction | All three client combinations completed private/public fork cases and compaction cases. | These fixture cases do not establish compatibility with every provider shape, client version, policy, or workload. |
| Claude Code child | A synchronous child completed through the checked lifecycle. | Client identity metadata remains an untrusted assertion. |
| OpenCode child | A synchronous child completed through the checked lifecycle. | Client identity metadata remains an untrusted assertion. |
| Codex V1 child | An asynchronous child receipt and checked return completed with public/private signed-ID corroboration. | A receipt outcome of `ack` records admission only. It does not prove a successful tool execution. |
| Parent audience | A private child return was admitted with its restrictions; the parent publish was then denied. | The result applies only to the exercised policy and fixture data. |
| Human review | One actual human approval completed through Claude Code. | The private approval evidence is not published. |
| Automated review | The authenticated test reviewer, `automated-test-reviewer`, completed review paths for Codex and OpenCode. | This synthetic reviewer is separate from, and cannot establish, human approval. |
| Older unmatched history | Old or unmatched provider history was refused. | The relay fails closed rather than reconstructing missing history. |
| Codex V2 | Native Codex V2 is unsupported. | A task name is not a supported V2 identity or lifecycle protocol. |
| Parallel roots | Parallel roots are unsupported. | The relay fails closed; this limitation is independent of Codex V2 support. |

The runtime's `logical_action_digest` and `review_scope` are request provenance metadata. They do not authenticate a reviewer or sign a response. The strict broker is a separate authority integration: it authenticates its reviewer, binds a ruling to its pending consultation, and writes its audit record before granting the ruling.

## Boundaries

- The package contains no provider keys, cookies, private keys, audit logs, SQLite state, raw captures, client transcripts, VM metadata, or external-service configuration.
- Tests use synthetic protocol values and local loopback servers only. They do not contact providers, models, or OpenAPPA runtimes.
- Relay logs and lifecycle state are runtime outputs. Supply protected locations explicitly. Do not write them into this source tree.
- `--keys-file` accepts only a mode `0600` regular JSON file with the supported provider-key fields. `APPA_PROVIDER_KEYS_FILE` and `APPA_LIFECYCLE_ANCHOR_KEY_FILE` may supply file paths only. They never accept key material.
- The strict broker binds only loopback and requires an explicitly configured authenticated reviewer.
- The fixture MCP is a test-only effect sink. `protected_publish` performs no authentication when called directly. Only the separately configured SourceGate/runtime and authority can protect an admitted path.
- The fixture MCP exposes only `read_source`, `publish`, and `protected_publish`. Client MCP permission examples scope only those synthetic tools. They do not describe native MCP permissions.

## Install And Run

Install from outside the checkout. This command does not install provider SDKs or contact a provider:

```sh
python3 -m venv /tmp/appa-stateful-proxy-venv
/tmp/appa-stateful-proxy-venv/bin/pip install --no-build-isolation /path/to/openappa/integrations/stateful-proxy
```

Keep mutable state outside the checkout and restrict it before use:

```sh
export APPA_STATE_DIR=/var/lib/appa-stateful-proxy
install -d -m 700 "$APPA_STATE_DIR"
export APPA_PROVIDER_KEYS_FILE="$APPA_STATE_DIR/provider-keys.json"
export APPA_LIFECYCLE_ANCHOR_KEY_FILE="$APPA_STATE_DIR/lifecycle-anchor.key"
chmod 600 "$APPA_PROVIDER_KEYS_FILE" "$APPA_LIFECYCLE_ANCHOR_KEY_FILE"
```

For lifecycle spawns, configure the documented provider-to-agent mapping, return floor, and MCP host. Replace the example values with policy-compatible values for the deployment:

```sh
export APPA_LIFECYCLE_SPAWN_TOOL_MAP='{"Agent":"agent:example/worker","spawn_agent":"agent:example/worker"}'
export APPA_LIFECYCLE_RETURN_FLOOR='{"trust":"trusted","audience":["public"]}'
export APPA_RUNTIME_URL=http://127.0.0.1:8787
export APPA_LIFECYCLE_MCP_HOST=127.0.0.1:8787
```

`APPA_RUNTIME_URL` is the source-runtime HTTP forward. `APPA_LIFECYCLE_MCP_HOST` is its matching MCP forward. Keep both loopback-only unless a secured local forward provides their remote transport. Run lifecycle enforcement with explicit protected output paths and the installed entry point. The key-file environment variables contain paths only.

```sh
appa-stateful-proxy \
  --archestra-base https://gateway.example \
  --keys-file "$APPA_PROVIDER_KEYS_FILE" \
  --logs "$APPA_STATE_DIR/logs" \
  --db "$APPA_STATE_DIR/mappings.sqlite3" \
  --appa-lifecycle-enforce \
  --appa-runtime "$APPA_RUNTIME_URL" \
  --appa-mcp-host "$APPA_LIFECYCLE_MCP_HOST" \
  --appa-trace "$APPA_STATE_DIR/decisions.jsonl" \
  --lifecycle-ledger "$APPA_STATE_DIR/lifecycle.sqlite3" \
  --lifecycle-gate-factory stateful_proxy.appa_lifecycle_gate:factory
```

Run the authenticated broker separately. It is the authority integration, not a response-signature verifier:

```sh
appa-stateful-strict-broker \
  --host 127.0.0.1 \
  --auth-url http://127.0.0.1:9000 \
  --reviewer-email reviewer@example.invalid \
  --audit-file "$APPA_STATE_DIR/reviews.jsonl" \
  --require-review-context
```

`--auth-url` is the locally forwarded Archestra application that implements BetterAuth's `/api/auth/get-session` endpoint. It is not a generic identity-provider URL.

The review relay also needs explicit paths and an explicit runtime MCP host:

```sh
appa-stateful-review-relay \
  --archestra-base https://gateway.example \
  --keys-file "$APPA_PROVIDER_KEYS_FILE" \
  --appa-runtime "$APPA_RUNTIME_URL" \
  --appa-mcp-host "$APPA_LIFECYCLE_MCP_HOST" \
  --logs "$APPA_STATE_DIR/review-logs" \
  --db "$APPA_STATE_DIR/review-mappings.sqlite3" \
  --appa-trace "$APPA_STATE_DIR/review-decisions.jsonl"
```

## Synthetic Fixtures

`fixtures/example/` contains only synthetic `public.txt`, `protected.txt`, and `policy.appa.toml`. Load the policy into the source runtime before the runner starts. The runner does not select or upload policy files.

The wheel and source distribution install these fixtures and `golden-scenarios.json` under the interpreter data directory. This finds the installed fixtures without a checkout:

```sh
export APPA_FIXTURES_DIR="$(python3 -c 'import sysconfig; print(sysconfig.get_path("data") + "/share/appa-stateful-proxy/fixtures/example")')"
```

With the source runtime and its MCP service available through the local forwards above, run the current-wire fixture harness outside the checkout:

```sh
appa-stateful-fixture-runner \
  --fixtures-dir "$APPA_FIXTURES_DIR" \
  --trace "$APPA_STATE_DIR/fixture-decisions.jsonl" \
  --summary "$APPA_STATE_DIR/fixture-summary.json" \
  --runtime-url "$APPA_RUNTIME_URL"
```

This runner uses `SourceGate` and the current `appa:execute_remedy_plan` control spelling. It does not use the legacy bare control spelling.

## Optional Client Probes

`appa-stateful-client-runner` packages the observed Claude Code, Codex, and OpenCode profile flags. It resolves client binaries and `appa-stateful-fixtures` from `PATH`, creates run state under `--work-root`, and configures the MCP fixture through that installed entry point.

The following command can contact a model through the configured relay. It is optional and is not part of the public test suite:

```sh
appa-stateful-client-runner codex \
  --work-root "$APPA_STATE_DIR/client-runs" \
  --proxy-url http://127.0.0.1:18765 \
  --fixture-mcp \
  --codex-v1-profile \
  --codex-read-only \
  --timeout 180
```

## Test Snapshot

Run source tests from this directory with Python 3.11+:

```sh
PYTHONPATH=src python3 -m unittest discover -s tests -v
```

The Python package suite contains 104 tests: 68 HTTP/SSE and provider-history tests, 19 client-metadata tests, 9 strict-broker tests, and 8 runner/package-configuration, fixture-protocol, and endpoint-isolation tests.

The private validation snapshot also recorded 132 artifact assertions and a historical full Rust result of 1,176 passed with 1 ignored. Those historical figures are evidence snapshots, not a claim about this pull request's current CI status.
