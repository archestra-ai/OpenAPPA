# Demo scenarios: APPA-gated kagent agents

These scenarios demonstrate OpenAPPA gating declarative kagent agents on Kubernetes. Every proposed flow crosses `appa-runtime` before tool execution.

The integration test suite in `../tests/` verifies twenty-four policy and adapter scenarios without requiring external LLM API access.

## Engine decision structure

OpenAPPA gates a flow at the point where a trajectory's label changes, not only at the final sink.

When a tool call is blocked, OpenAPPA returns structured feedback with a remedy offer:

```text
[appa] Blocked: this call cannot run yet.
Why:
  - session trust would fall: trusted -> suspicious
Continue:
  - Accept this change for the rest of this session:
    execute_remedy_plan(offer_id: "…")
```

The agent can accept the label change or execute an alternative remedy plan.

## Scenario 1: Confidential read and sanitization

This scenario prevents confidential data from leaking into public destinations.

The agent attempts to read a secret:

```text
read_secret(name: "payments-provider")
```

1. **Policy rule:**
   `read_secret` specifies `delta = { audience = ["ops"] }`.

2. **Engine decision:**
   The trajectory started with a public audience. Admitting the secret narrows the audience to `ops`. OpenAPPA blocks the call before credentials enter model context.

3. **Resolution:**
   OpenAPPA returns a continuation offer. The agent calls `execute_remedy_plan` with the `strip-secret-values` sanitizer. Safe key names return to the model without credentials.

## Scenario 2: Untrusted ingress and prompt injection

This scenario prevents prompt injection payloads from entering a trusted trajectory silently.

The agent attempts to read crash logs containing prompt injection payloads:

```text
get_pod_logs(name: "checkout-api-b2k1")
```

1. **Policy rule:**
   `get_pod_logs` specifies `delta = { trust = "suspicious" }`.

2. **Engine decision:**
   Admitting the unvetted log content reduces trajectory trust from `trusted` to `suspicious`. OpenAPPA blocks the read.

3. **Resolution:**
   The agent can accept the trust reduction, or delegate log inspection to the `log-analyst` child agent. The child reads the log on an isolated trajectory and returns a clean summary.

## Scenario 3: Human review for destructive actions

This scenario enforces human sign-off for operational actions using native kagent confirmation cards.

The agent attempts to restart a deployment:

```text
restart_deployment(name: "checkout-api")
```

1. **Policy rule:**
   `restart_deployment` specifies `requires = { attention = ["human-approval"] }`.

2. **Engine decision:**
   Only the `oncall` human authority can grant `human-approval`. OpenAPPA blocks the direct call and offers a remedy plan referencing `oncall`.

3. **Resolution:**
   The agent calls `execute_remedy_plan`. The turn suspends, and an **Approve / Reject** card appears in the kagent dashboard:
   - **Approve**: The `oncall` authority grants approval, and the restart runs.
   - **Reject**: The `oncall` authority denies approval, and the action stops.

## Scenario 4: Remote authority review

This scenario demonstrates asynchronous review by an external change advisory board.

The agent attempts to rollback a deployment:

```text
rollback_deployment(name: "checkout-api")
```

1. **Policy rule:**
   `rollback_deployment` specifies `requires = { attention = ["change-approval"] }`.

2. **Engine decision:**
   The policy delegates `change-approval` to the external `change-board` authority via an HTTP webhook. OpenAPPA suspends the call while waiting for a decision.

3. **Resolution:**
   An external operator reviews the request via the change board API (`GET /pending`, `POST /decide`). An approval ruling allows the rollback to execute.

## Scenario 5: Data boundaries with the GitHub battery

This scenario establishes information-flow boundaries on repository tools.

1. **Policy rule:**
   The GitHub battery labels repository contents as `suspicious`. It requires `trusted` data for `mcp__github__issue_write`.

2. **Engine decision:**
   Reading repository files succeeds. However, forwarding that unvetted repository text to `issue_write` is blocked because suspicious data cannot flow into trusted sinks.

3. **Resolution:**
   Fresh text supplied directly by an authorized operator retains `trusted` status and publishes without human approval.

## Scenario 6: Permitted reads flow without interruption

An ordinary query carries no audience or trust restrictions:

```text
list_pods(namespace: "shop")
```

OpenAPPA evaluates the call against policy. Because no label boundaries are violated, the call executes immediately without human interruption.

## Running the scenarios locally

To execute the full integration test suite deterministically without an LLM API key:

```sh
APPA_INTEGRATION=1 uv run --project integrations/kagent/appa-kagent-adk \
  --with "kagent-adk @ git+https://github.com/kagent-dev/kagent@v0.9.12#subdirectory=python/packages/kagent-adk" \
  --with "a2a-sdk>=0.3.23,<0.4" --with "google-adk==1.31.1" --with "mcp>=1.25,<2" --with "pytest>=8" \
  pytest integrations/kagent/tests
```
