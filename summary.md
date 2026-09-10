# OpenAPPA integration with Archestra

This document records the agreed target architecture. Add clarifications only after the user approves them.

1. **Runtime:** call OpenAPPA through TypeScript–Rust bindings inside Archestra, without a separate HTTP service.
2. **Integration points:** call existing OpenAPPA hooks from Archestra’s existing LLM proxy. Add hooks only if necessary.
3. **Session identity:** use session and parent IDs supplied by the client adapters.
4. **Duplicate results:** Archestra tracks which tool results OpenAPPA has already processed.
5. **Result replacements:** Archestra saves approved replacements and reapplies them whenever clients resend the original results.
6. **Policy state:** OpenAPPA evaluates rules and maintains restrictions across requests and compaction.
7. **Remedies:** reuse OpenAPPA’s existing remedy plans and MCP execution mechanism.
8. **Persistence:** use Archestra’s PostgreSQL and migration system, with dedicated OpenAPPA tables where needed. OpenAPPA owns its storage format.

## 1. Runtime — accepted clarification

Use the same native binding approach as Archestra’s Dagger integration: `@archestra/sandbox-rs` uses napi-rs to expose Rust functions to TypeScript.

- Add an equivalent package, such as `@archestra/openappa-rs`, linking the existing OpenAPPA Rust runtime. The package name is illustrative.
- Load the native module inside Archestra’s backend and keep the runtime initialized across requests.
- Expose an asynchronous binding to the existing OpenAPPA hook dispatcher. The LLM proxy passes events such as `SessionStart`, `ToolCall`, and `ToolResult`, and awaits their decisions.
- Compile and package the native binary using the same build and loading pattern as `sandbox-rs`.
- The call path is: **existing LLM proxy → native binding → existing OpenAPPA hook → decision returned to TypeScript**.
- No separate OpenAPPA HTTP server is needed. The binding is new integration code; the existing hooks and policy engine are reused.
- PostgreSQL support is a separate change. The binding alone does not replace OpenAPPA’s current SQLite storage integration.

## 2. Integration points — accepted clarification

Archestra’s existing LLM proxy calls OpenAPPA’s existing hook dispatcher through native bindings:

- **`SessionStart`:** open or restore the conversation’s OpenAPPA state before checking its calls or results.
- **`ToolCall`:** check a model-proposed tool call before releasing it to the client. Apply the allow/block decision and provide any remedy offers to the model. The client executes allowed calls. Remedy calls also pass through this check before MCP execution.
- **`ToolResult`:** check each new tool result before sending it to the model. Apply any output replacement. Previously checked results reuse saved replacements instead of triggering the hook again.
- **`Prompt`:** report a new user turn when the client adapter can reliably identify it. This marks a turn boundary; it does not check the prompt text against policy.
- **`TurnEnd`:** report a completed agent turn when its completion is known, so OpenAPPA can clean up unfinished calls and temporary permissions.
- **`ChildStart`, `ChildEnd`, and `SpawnResult`:** report child creation, check the child’s return, and report the parent’s spawning-tool result when the adapter can identify these events and their relationship.

Archestra already buffers streamed tool calls until they are complete, evaluates guardrails, and releases only allowed calls. Reuse this mechanism, replacing the existing guardrail evaluation with OpenAPPA’s `ToolCall` hook. Streaming is an existing capability to preserve, not a gap.

### Potential gaps

- **Turn boundaries:** an inference request is not an entire agent turn. For example, the model requests a tool, the client runs it, and the client sends another model request—all within the same agent turn. If the user stops the client while a tool is running, the proxy may receive neither the result nor a “stopped” event. Completing an HTTP request therefore does not justify calling `TurnEnd`; Archestra needs a reliable signal that the client finished or stopped the turn. Likewise, call `Prompt` only for an identified new user turn, not every model request.
- **Tool outcomes:** reuse Archestra’s existing execution-status and error handling to supply success, failure, or uncertain outcomes to OpenAPPA’s `ToolResult` hook. Do not introduce a separate classifier. During implementation, check which existing paths cover external clients; leave any unavailable status unknown.
- **Children:** use session/parent IDs and the observed spawning call to associate each child with its approved spawn, reusing OpenAPPA’s existing association logic where possible. This selects the correct inherited restrictions and return requirements. A single pending spawn can be associated without another client-supplied ID, as in the Claude adapter. Multiple pending spawns need an unambiguous association. The client adapter must also identify which output is the child’s return to its parent so OpenAPPA can check it before it reaches the parent.
- **Compaction:** summarization must use approved results. After compaction, retain restrictions by session ID and replace only tool results still present.

The existing hooks cover the tool-call/result sequence. The unresolved work is obtaining reliable lifecycle information from each client and defining behavior when that information is missing.

## 3. Session identity — accepted clarification

Use the header contract supplied by the client adapters:

- **`X-Appa-Session-ID`:** the current session ID, stable across model requests, user messages, and compaction. A child has its own session ID.
- **`X-Appa-Parent-ID`:** the parent session ID for a child session; omitted for a top-level session.

For the initial implementation, have Archestra Chat send these headers to the existing LLM proxy. The proxy uses these identities when calling OpenAPPA. Assume other client adapters will supply the same header contract; implementing their identity detection is outside this initial work.

## 4. Duplicate results — accepted clarification

Archestra tracks processed tool results in its existing PostgreSQL database, keyed by organization, authenticated caller, session ID, and tool-call ID. Reuse existing tables where practical; otherwise add a dedicated record containing that key, processing status, and approved output.

- **New result:** call OpenAPPA’s `ToolResult` hook and save the processed status and approved output.
- **Already processed result:** skip the hook and always substitute the saved approved output. Do not repeat policy updates or annotator/sanitizer calls just because the client resends history.
- **No content hash initially:** deduplication uses the record key. Even if the client resends different content under that key, only the saved approved output reaches the model. A content hash would be an optional consistency check, not a requirement for this behavior.

For example, if two consecutive requests both include the result for `call_123`, only the first processes it through OpenAPPA; the second reuses its approved output.

Concurrent requests must not process the same result twice. Saving OpenAPPA state and the processed-result record must be coordinated so they remain consistent after a restart. The transaction and interrupted-processing behavior will be defined under Persistence.

## 5. Result replacements — accepted clarification

As agreed in point 4, Archestra saves each tool result’s approved output and uses it in model requests, both when first checked and whenever the result is resent. Duplicate detection skips repeated OpenAPPA processing; replacement determines what the model receives. These are two parts of the same result-handling flow.

## 6. Policy state — accepted clarification

OpenAPPA owns policy evaluation and the conversation’s restriction state. Archestra supplies session identities and hook events. Connect OpenAPPA’s persistence to Archestra’s PostgreSQL, preserving OpenAPPA’s existing state semantics rather than implementing a second policy-state system in TypeScript.

Restrictions remain attached to the session across requests, compaction, and restarts. Removing a tool result from model history does not remove the restrictions that resulted from processing it. Stored approved tool outputs are separate from this conversation-level policy state.

The PostgreSQL storage implementation and transaction details will be defined under Persistence.

## 7. Remedies — accepted clarification

Reuse OpenAPPA’s existing remedy plans and `execute_remedy_plan` MCP tool. Expose the tool through Archestra’s MCP gateway. The model chooses an offered plan, and the client executes the tool through the gateway; OpenAPPA validates and executes the plan.

For simplicity, consider a dedicated gateway exposing only the remedy-plan tool. This is an option, not a finalized requirement. Gateway routing must reach the same OpenAPPA runtime and session state used by the LLM proxy.

## 8. Persistence — accepted clarification

Archestra owns the PostgreSQL database and runs table migrations. OpenAPPA owns its event format, serialization, and rebuilding policy state from stored events. Archestra does not need to interpret those events.

- Add PostgreSQL support to OpenAPPA’s storage layer.
- Create the required tables through Archestra’s migration system. OpenAPPA currently persists ordered event batches and the original policy files they reference. Dedicated tables could be named `openappa_events` and `openappa_policy_files`; these names are illustrative.
- Store each event batch with its trajectory ID and sequence number, and each policy file with its hash and original bytes. Keep event encoding and decoding in OpenAPPA’s Rust code. PostgreSQL can store the serialized bytes without TypeScript interpreting them.
- Preserve OpenAPPA’s ordering and concurrent-write checks when implementing PostgreSQL storage.
- If a storage update needs different columns, include the corresponding Archestra migration. A change inside a serialized event does not necessarily require changing table columns, but OpenAPPA must preserve compatibility with saved history or provide a conversion.
- Keep OpenAPPA’s policy history separate from Archestra’s chat and request logs. Archestra’s processed-result records and approved outputs remain its integration responsibility, as described in points 4 and 5.

All eight points now have accepted clarifications. Detailed lifecycle mapping and the transaction behavior for interrupted or concurrent processing remain implementation design work.

---

## Implementation record — 2026-09-10

Added under the subsequent instruction to implement, verify in Chrome, and
record the built architecture and gaps. The accepted target above is preserved.

### Isolation and implementation sequence

The original checkouts and previous prototype worktrees were preserved. Work
started from clean, paired worktrees:

| Repository | Branch | Base | Worktree |
| --- | --- | --- | --- |
| OpenAPPA | `feat/archestra-native-postgres` | `91d432e1` | `/private/tmp/archestra-openappa-native/openappa` |
| Archestra | `feat/openappa-native` | `fd32e2580` | `/private/tmp/archestra-openappa-native/archestra` |

The inspection led to this sequence: add optional PostgreSQL storage and the
host transaction seam; add the native package and Archestra migration; connect
the existing proxy and Chat execution/lifecycle paths; expose the existing
remedy mechanism; verify native persistence, proxy streaming, and live Chat.
The decisions that inspection left open were interruption recovery, trustworthy
caller identity, uncertain outcomes, child-return mapping, and packaging the
coordinated repositories. Their implemented choices and limits follow.

### Architecture that was built

- `@archestra/openappa-rs` is a persistent async napi-rs addon using the existing
  `@archestra/napi-loader` pattern. It calls `hooks::handle` on the real OpenAPPA
  runtime. No OpenAPPA HTTP service is started. `Runtime::open_with_store`
  accepts shared host-managed storage without replacing the policy engine.
- The existing LLM proxy calls `SessionStart`, checks submitted results, and
  calls `ToolCall` in both its streaming and non-streaming guardrail positions.
  Its existing complete-call buffering and release/refusal behavior remain.
  Existing normalization handles decorated names and `run_tool` targets.
- Chat sends `X-Appa-Session-ID` from its conversation ID. The model constructor
  supports `X-Appa-Parent-ID`; top-level Chat omits it. Organization and caller
  are authenticated and included in the native actor identity. Chat signs the
  proxy-agent/user/session/parent tuple with Archestra's existing auth secret
  for the internal HTTP hop. A loopback connection or user attribution header
  alone cannot authenticate a policy session. The internal signature header is
  redacted and is not sent to the LLM provider.
- Chat supplies `Prompt` for an identified new user message and `TurnEnd` on
  non-aborted UI stream completion. These are not emitted per inference.
  Unknown cancellation is left to the existing next-prompt cleanup. Detached
  MCP tasks are disabled while this initial Chat adapter is enabled.
- Chat checks the durable call receipt again immediately before execution,
  including resumed approval calls. It captures the existing MCP execution
  outcome before formatting, checks output before returning it to the AI SDK,
  and saves only approved text into conversation history. The proxy substitutes
  the saved approved output on every history resend.
- The built-in `archestra__execute_remedy_plan` gateway tool uses OpenAPPA's
  existing MCP execution logic through `execute_embedded_remedy`. It is implicit
  protocol support while enabled, following Archestra's existing run-control
  pattern. Each execution still checks the control call and validates the
  offer against the authenticated native actor. Remote authority/sanitizer
  bindings use the actual runtime implementation.
- Archestra migration `0463_openappa_native.sql` creates `openappa_events`,
  `openappa_policy_files`, `openappa_sessions`, `openappa_operations`, and
  `openappa_processed_results`. Rust owns event bytes, original policy bytes,
  decoding, replay, ordering, and compare-and-swap checks. TypeScript does not
  interpret policy events. Existing SQLite storage remains supported.

### Persistence decisions

A dedicated PostgreSQL connection thread supports OpenAPPA's existing sync
store API under async hooks. Host receipt SQL and native event writes use the
same connection. The initial binding serializes hooks within a process and
uses a PostgreSQL advisory lock per trajectory family across processes.

Before an operation that might consult an external authority, the binding
commits a `pending` receipt. All hook event writes and the completed receipt
then share one transaction. Success commits both. Failure rolls back both and
rebuilds transient runtime state. A surviving pending receipt blocks the family
after interruption, including after process restart. An operator must inspect
the event/receipt and external authority history before recovery; there is no
automatic reset or blind consult retry.

Completed result keys are organization + authenticated caller + session +
tool-call ID. A changed resend still gets the saved approved output, with no
additional hook or sanitizer invocation. Call operation IDs reject changed
inputs. Result correlation uses the stored original checked call, so it does
not depend on that call still appearing in compacted model history.

This coordinates database state and avoids repeating uncertain external work.
It does **not** claim exactly-once side effects at arbitrary remote services:
their result may be lost before the database transaction commits.

### Verified behavior

| Check | Observed result |
| --- | --- |
| OpenAPPA runtime unit suite | 499 passed |
| Existing event-store suite | 18 passed |
| PostgreSQL parity/concurrency/transaction test | Passed against migrated PostgreSQL 17 |
| Native Node/PostgreSQL suite | 6 passed, including cross-process replay and an actual HTTP sanitizer |
| Backend regression suites | 261 passed across 8 files, including 9 new proxy/identity tests |
| Backend TypeScript and changed-file Biome checks | Passed |
| Archestra migration chain and migration checks | Applied successfully; 0 errors, 208 linter warnings, including index warnings on the new empty tables |
| Native build and async loader smoke | Built and loaded on macOS arm64 |
| Container source selection | Matching isolated source selection passed Cargo metadata and `cargo check --locked` |

The native tests cover concurrent duplicate results, changed resends, caller/
tenant isolation, remedy acceptance, retained restrictions after a fresh
process receives no prior model history, one-time sanitization with saved
replacement replay, unknown outcomes, and a failed receipt update that rolls
back event writes and leaves a durable pending record.

Chrome MCP was used against the isolated Archestra UI at
`http://127.0.0.1:13000`. **GPT-5.6 Terra was selected in the Chat dropdown**;
the database recorded `gpt-5.6-terra` for the live requests. The supplied API key
was entered through Archestra's provider form and was not put in source or this
document. Initial checks used
`/chat/3a5cdee1-0502-427e-9dc5-b815885f3dbe`; the final normal-discovery test used
`/chat/50b7487c-e365-4b91-b134-119f3340cbc5`.

Live Chat returned the requested text, blocked `archestra__whoami` with a native
remedy offer, executed the offered remedy through the MCP gateway, and returned
the actual agent identity after retry. The test agent's read-only tools were
configured through Archestra's existing assignment API. Its Auto-mode
exclusions initially kept those tools unavailable; switching this disposable
test assistant to Custom access resolved that configuration issue. Failed
executions did not incorrectly apply a successful-read restriction.

The final test admitted the existing read-only `archestra__search_tools` step,
blocked `whoami`, executed the offered acceptance remedy, and returned the real
agent identity. After restarting the backend (listener PID changed from 11539
to 14657), `archestra__list_agents` was blocked with **"trust is suspicious,
below the required floor trusted"**. PostgreSQL recorded that exact native
deny decision, eight `gpt-5.6-terra` interactions for the conversation, and three
completed result receipts with no pending result. The separate native test
covers the stronger case of restarting with no prior model history supplied.

An earlier browser probe stopped at the model's tool-discovery requirement,
before attempting a tool. It was not evidence of native policy enforcement.
Adding the discovery tool to the test fixture and starting a fresh conversation
resolved it. An optional change to advertise all assigned schemas was rejected
by automatic approval review and was not applied; the final test used the
existing discovery path.

### Gaps and corrections to assumptions

1. **Unknown is not success.** Several proxy adapters default `isError` to false.
   That is not proof of execution success. Chat supplies the real MCP outcome;
   other clients remain unknown when no trustworthy status is available.
   Existing OpenAPPA `Indeterminate` returns Keep/Ack, so the binding explicitly
   withholds an unknown body's text instead of forwarding unevaluated content.
2. **A proxy refusal ends the model step.** The existing refusal mechanism shows
   and saves the remedy offer. It reaches the model on the next user turn;
   automatic same-turn remedy continuation is not implemented. This is a limit
   of the preserved proxy behavior, not missing streaming support.
3. **Registering a remedy tool was insufficient.** Ordinary agent assignment
   initially hid it. The implementation now advertises and permits the enabled
   control tool implicitly, with native session/offer authorization intact.
4. **Child hooks are not a finished Chat child adapter.** Native child-start,
   child-end, and spawn-result plumbing reuses existing hooks, but Chat does
   not yet deliver the full child return contract. Agent/skill delegation is
   refused while enabled. Native feedback can still suggest child delegation
   as an alternative; that suggestion is not usable in this initial Chat flow.
5. **Interactive elicitation needs a bridge.** Existing bound remote remedies
   work; interactive MCP human review has no embedded request-context bridge
   yet and uses the runtime's existing no-answer behavior.
6. **Locked chats and rich results are limited.** Locked Chat is refused because
   native tables do not yet use its browser-held encryption keys. Approved text
   is supported; raw MCP UI content, images, and structured output are omitted
   so they cannot bypass output replacement during history or compaction.
7. **Start new conversations when enabling.** Old results without native call
   receipts are refused. Prototype/SQLite histories are not silently migrated.
   Other client lifecycle detection and provider-hosted tool execution are not
   implemented by this initial Chat adapter.
8. **Deployment work remains explicit.** Local builds require matching sibling
   checkouts. Docker uses a named `openappa` source context and the normal native
   build/deploy path. The full Linux/musl platform image was not built in this
   verification; the copied source selection was checked on the host. CI must
   supply/pin the matching OpenAPPA checkout when these branches are adopted.
9. **Operational follow-ups remain.** Dispatch is initially serialized per
   process; connection recovery currently requires backend restart. Retention,
   user/organization purge integration, and an interrupted-operation recovery
   UI are not provided. PostgreSQL uses system TLS trust and URL SSL settings;
   deployment-specific client certificate options need validation.
10. **Existing checks still exist.** OpenAPPA replaces legacy proxy evaluation.
    Chat/gateway access controls and their own execution checks remain active.
    Enabling OpenAPPA does not grant access to otherwise unavailable tools.

A pre-existing circular import between Archestra's built-in-agent constants
and system-prompt templates prevented the fresh UI from loading. Extracting the
dependency-free IDs fixed that startup prerequisite. Dev UI React warnings
unrelated to OpenAPPA were observed. An early claim during test debugging that
provider validation stripped identity headers was incorrect: the fixture user
lacked organization membership. No header-schema workaround was retained.

The detailed build/setup/transaction notes and reproducible commands are in
`archestra/platform/archestra-rs/openappa-rs/README.md` in the paired worktree.
