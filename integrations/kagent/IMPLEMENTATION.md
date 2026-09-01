# kagent dynamic extension implementation plan

Source baseline:

- kagent commit `9e246fd3797457b18fc277680be1629a0f57fce0`
- Google ADK Go `v2.2.0` at `b264039aaec43baedc123e5b9a0cf87681d0bbca`
- Substrate `v0.0.20`
- OpenAPPA `origin/main` at `77230bbb177d72ac740173036b715ff2b3f1ae24`

The reader-facing proposal is at [openappa.com/kagent](https://www.openappa.com/kagent).

## Accepted design decisions

The human reviewer selected these constraints:

1. Run the dynamic OpenAPPA extension as a digest-pinned OCI companion container.
2. Permit generic non-OpenAPPA lifecycle infrastructure when ADK callbacks cannot cover readiness, egress, snapshots, or process lifecycle.
3. Classify a crash after execution acknowledgement and before terminal completion as `Indeterminate` inside OpenAPPA.
4. Pin one extension revision per live scope and drain old scopes during upgrades.
5. Publish the OpenAPPA proposal and plugin, plus a separate kagent generic-host PR to a private fork.
6. Keep Google ADK Go as an unmodified upstream dependency.

These decisions are normative for the first implementation and PoC.

## Goal

Add a generic dynamically supplied extension system to the kagent fork.

Ship OpenAPPA as an independently built sidecar that uses this generic system.

Use only public Google ADK interfaces and kagent-owned integration points. Do not fork, vendor, copy, patch, replace, or import internal Google ADK packages.

If those surfaces cannot enforce a required boundary, report that capability as unavailable and refuse protected-workload activation.

The kagent fork MUST contain no OpenAPPA logic or OpenAPPA-specific data model.

The combined system MUST preserve these invariants:

1. kagent executes only a matching immutable payload from a valid generic `permit` decision.
2. Raw execution output reaches no protected sink before a matching generic `commit` decision.
3. Every content-bearing model, session, child, A2A, memory, UI, log, telemetry, and storage path crosses an exclusive generic gate.
4. Extension ordering and phase ownership are immutable for one Revision.
5. Required extension failure blocks readiness or freezes the affected scope.
6. kagent never parses a plugin policy, fact, Label, remedy, consult, or extension-specific state.
7. The OpenAPPA sidecar owns all OpenAPPA state, semantics, recovery, and external consults.
8. One extension artifact and protocol selection remain pinned for each live scope.
9. No invariant depends on a change to Google ADK source.

## Evidence and adopted patterns

The design follows primary sources from widely adopted plugin systems.

| Source | Applied design |
|---|---|
| [HashiCorp go-plugin](https://github.com/hashicorp/go-plugin) | Out-of-process RPC, handshake, protocol majors, health, and process isolation |
| [go-plugin internals](https://github.com/hashicorp/go-plugin/blob/v1.8.0/docs/internals.md) | Explicit launch handshake and compatible protocol selection |
| [Terraform plugin protocol](https://developer.hashicorp.com/terraform/plugin/terraform-plugin-protocol) | Separate artifact versions and wire compatibility |
| [Kubernetes device plugins](https://v1-32.docs.kubernetes.io/docs/concepts/extend-kubernetes/compute-storage-net/device-plugins/) | Serve before register, Unix-socket gRPC, capability discovery, and re-registration |
| [VS Code extension manifest](https://code.visualstudio.com/api/references/extension-manifest) | Declarative capabilities, compatibility ranges, activation, and contribution points |
| [Envoy xDS](https://www.envoyproxy.io/docs/envoy/latest/api-docs/xds_protocol) | Versioned resources and explicit ACK or NACK |
| [Docker plugin configuration](https://docs.docker.com/engine/extend/config/) | Reviewable image, socket, mount, network, device, and capability requests |
| [gRPC health checking](https://grpc.io/docs/guides/health-checking/) | Standard sidecar health protocol |
| [Buf breaking checks](https://buf.build/docs/breaking/) | Protocol compatibility enforcement in CI |

Do not use Go's standard [`plugin`](https://pkg.go.dev/plugin) package. It has strict toolchain coupling, shared memory, platform limits, no unload, weak race-detector support, and no independent lifecycle.

## Current source constraints

Current public ADK and kagent surfaces lack the full required boundary set.

| Requirement | Source evidence | Allowed integration path |
|---|---|---|
| Dynamic plugin object | [kagent runner adapter](https://github.com/kagent-dev/kagent/blob/9e246fd3797457b18fc277680be1629a0f57fce0/go/adk/pkg/runner/adapter.go#L74-L95) | Install one generic out-of-process proxy through the existing plugin surface |
| Provider-final catalog | Public model interface and [kagent Bedrock adapter](https://github.com/kagent-dev/kagent/blob/9e246fd3797457b18fc277680be1629a0f57fce0/go/adk/pkg/models/bedrock.go#L730-L808) | Add the gate to kagent-owned adapters; refuse opaque upstream providers |
| Pre-plugin dispatch | Existing public tool callbacks | Install the proxy first and disable later untrusted callbacks in protected workloads |
| Event persistence | Public session service and Runner output | Wrap both surfaces in kagent; refuse any direct backing-store or yield path |
| User input gate | Existing public `OnUserMessageCallback` | Route it through the dynamic proxy and session wrapper |
| Child terminal gate | Public Agent, Runner, and session interfaces | Correlate child scopes in kagent wrappers; refuse unobservable paths |
| Automatic memory | Public memory and tool interfaces | Disable stock preload and use a gated kagent-owned implementation |
| MCP transport | Public tool interface | Use a kagent-owned no-retry transport; refuse the stock opaque client |
| Remote A2A | [kagent tool](https://github.com/kagent-dev/kagent/blob/9e246fd3797457b18fc277680be1629a0f57fce0/go/adk/pkg/tools/remote_a2a_tool.go#L25-L145) | Wrap request and result handling in the kagent fork |
| Content telemetry | Public configuration plus [kagent attributes](https://github.com/kagent-dev/kagent/blob/9e246fd3797457b18fc277680be1629a0f57fce0/go/adk/pkg/telemetry/attributes.go#L37-L47) | Disable upstream content capture and use content-free kagent wiring |
| Readiness | [kagent A2A server](https://github.com/kagent-dev/kagent/blob/9e246fd3797457b18fc277680be1629a0f57fce0/go/adk/pkg/a2a/server/server.go#L127-L144) | Gate kagent readiness on generic extension recovery |
| Snapshot lifecycle | [kagent ActorTemplate](https://github.com/kagent-dev/kagent/blob/9e246fd3797457b18fc277680be1629a0f57fce0/go/core/v2/substrate/actor_template.go#L65-L88) | Add generic lifecycle handling outside ADK |

Source links into Google ADK explain observed behavior only. They are not implementation targets.

Substrate can run [up to ten peer containers](https://github.com/kagent-dev/substrate/blob/v0.0.20/pkg/api/v1alpha1/actortemplate_types.go#L243-L305).

It gates Actor readiness on configured probes. Pinned kagent currently emits one workload container.

The baseline Container API lacks several required primitives.

These include shared memory, projected configuration, per-container identity, filesystem controls, seccomp, and enforced egress.

Add these as generic Substrate primitives. The compiler and Actor activation MUST reject an extension whose required primitive cannot be enforced.

## Ownership and forbidden dependencies

### kagent fork owns

- Generic extension API types and generated gRPC client.
- Generic manifest validation and immutable Revision compilation.
- Generic sidecar container, volume, socket, secret, egress, readiness, and snapshot rendering.
- Dynamic proxies for existing public ADK callbacks.
- Wrappers for public ADK model, tool, session, memory, Agent, and Runner interfaces.
- Immutable descriptor and payload snapshots in kagent-owned adapters.
- Generic execution proxy and sink barriers.
- Generic interaction and remote metadata relay.
- Host-native ADK session and A2A task state.

### OpenAPPA plugin owns

- OpenAPPA policy parsing and `[deployment]` validation.
- Engine and runtime adapter behavior.
- OpenAPPA facts, call state, consult evidence, offers, and remedies.
- OpenAPPA-specific readiness and host-inventory validation.
- OpenAPPA-specific dynamic tools.
- Annotator, Membership Resolver, Authority, and Sanitizer calls.
- OpenAPPA interaction, delegation, recovery, and snapshot state.
- Plugin-side durable database and migrations.

### kagent fork MUST NOT contain

- A package, module, field, enum, callback, table, or error code named for OpenAPPA.
- OpenAPPA policy, Label, trajectory, ReaderId, Authority, Annotator, Sanitizer, Membership, remedy, effect, attention, or fact types.
- OpenAPPA tool names or special-case routing.
- OpenAPPA database paths or schema migrations.
- OpenAPPA readiness, coverage, consult, or audit logic.
- A Google ADK fork, vendored or copied ADK source, replacement module, source patch, or import from an ADK `internal` package.

Add CI rules that reject imports and identifiers from a maintained forbidden list in the kagent generic-host packages.

## Repository deliverables

### kagent private fork

| Owner | Generic change |
|---|---|
| `go/api/v1alpha3` | Add ordered dynamic extension declarations to Harness |
| `go/core/v2/translator` | Validate generic manifests, references, capabilities, and immutable Revision data |
| `go/core/v2/substrate` | Render multiple extension containers, private sockets, projected config and secrets, state volumes, egress, and probes |
| `go/adk/pkg/extensions/protocol` | Generated neutral protobuf messages and client |
| `go/adk/pkg/extensions/host` | Negotiation, ordering, health, deadlines, retries, and generic decisions |
| `go/adk/pkg/runner` | Construct the dynamic proxy as the only content-bearing public ADK callback path |
| `go/adk/pkg/tools` | Generic immutable execution proxy and remote request wrappers |
| `go/adk/pkg/a2a` | Generic interaction relay and pre-publication gate |
| `go/adk/pkg/session` | Generic pre-append gate and delivery deduplication |
| telemetry | Enforce immutable content-free protected workload settings before provider construction |

### OpenAPPA repository

| Owner | OpenAPPA component |
|---|---|
| `appa-kagent-plugin-protocol` | OpenAPPA-side generated client/server binding to the neutral protocol |
| `appa-kagent-plugin` | gRPC sidecar, generic host-phase mapping, readiness, dynamic tools, interactions, and recovery |
| `appa-runtime-api` | Missing response emission, structured remedy, MRTR, and exact dispatch APIs required by the plugin |
| `appa-runtime` | Engine session ownership, consults, durable plugin state, and protocol mapping |
| OCI packaging | Signed sidecar image, manifest, SBOM, provenance, and migrations |
| Integration tests | Fake generic host plus full kagent/sidecar end-to-end suite |

## Generic Harness API

Add an ordered extension list to the Harness workload declaration:

```yaml
workload:
  image: ghcr.io/private-kagent/kagent-go-adk@sha256:<digest>
  extensions:
    - name: policy-gate
      image: ghcr.io/archestra-ai/openappa-kagent-plugin@sha256:<digest>
      manifestDigest: sha256:<digest>
      signaturePolicyRef: required-extension-signers-v1
      required: true
      failureMode: closed
      order: 10
      protocol:
        minMajor: 1
        maxMajor: 1
      socket:
        path: /run/kagent/extensions/policy-gate.sock
      config:
        configMapRef:
          name: customer-support-policy-v1
      secrets:
        - secretRef:
            name: policy-gate-secrets-v1
      state:
        volumeName: policy-gate-state
        mountPath: /var/lib/kagent-extension
      egress:
        - host: api.policy-provider.example
          port: 443
      readiness:
        path: /readyz
        port: 8082
      resources:
        cpu: "2"
        memory: 2Gi
```

The kagent compiler treats `config` and secret content as opaque.

It validates reference scope, signatures, digests, volume ownership, egress shape, protocol ranges, resource limits, socket uniqueness, and ordering.

For every ConfigMap and Secret reference, the generic controller reads bytes only to hash and copy them. It never parses their plugin meaning.

Create immutable Revision-owned ConfigMap and Secret copies with generated names bound to content digest, source UID, and source resource version.

Store only digests and generated object references in the Revision. Do not store secret bytes in the Revision or ActorTemplate.

Mount only the immutable generated copies into the extension container.

Admission prevents update or deletion while a live Revision references the copy.

Policy, provider credential, CA, or plugin secret rotation always creates a new Revision and follows pin-and-drain.

Per-activation mTLS and message-authentication credentials are separate ephemeral boot credentials. They cannot replace immutable plugin configuration.

The immutable Revision stores canonical generic extension declarations and provenance.

## Generic extension manifest

Each image contains a signed manifest:

```json
{
  "schema_version": 1,
  "plugin_id": "org.example.policy-gate",
  "artifact_version": "0.1.0",
  "artifact_digest": "sha256:...",
  "protocol": { "min_major": 1, "max_major": 1 },
  "state_schema_version": 1,
  "capabilities": {
    "required": [
      "catalog.logical.v1",
      "catalog.provider_final.v1",
      "model.propose.v1",
      "tool.propose.v1",
      "execution.begin.v1",
      "tool.complete.v1",
      "event.gate.v1",
      "session.persist.v1",
      "a2a.publish.v1",
      "remote.request.v1",
      "child.lifecycle.v1",
      "memory.lifecycle.v1",
      "mcp.transport.v1",
      "interaction.relay.v1",
      "snapshot.lifecycle.v1"
    ]
  },
  "phase_claims": {
    "tool.propose": "exclusive",
    "tool.complete": "exclusive",
    "model.propose": "exclusive",
    "model.event": "exclusive",
    "session.persist": "exclusive",
    "a2a.publish": "exclusive",
    "remote.request": "exclusive"
  },
  "limits": {
    "max_inline_bytes": 1048576,
    "max_callback_ms": 5000,
    "max_interaction_ms": 86400000
  },
  "failure_mode": "closed"
}
```

Manifest capabilities describe host mechanics only.

The compiler rejects duplicate plugin IDs, phase conflicts, unsupported required capabilities, invalid failure modes, excessive limits, or signature mismatch.

## Protocol services

Define `kagent.extension.v1` with these generic RPCs:

| RPC | Purpose |
|---|---|
| `GetExtensionInfo` | Return artifact, protocol, manifest, and state schema identity |
| `Negotiate` | Select protocol and capabilities for one host inventory |
| `Activate` | Accept the immutable Revision and host inventory digest |
| `InvokePhase` | Process one immutable generic host or lifecycle event |
| `Quiesce` | Stop new plugin work and commit one state generation |
| `ValidateRestore` | Accept or reject a restored host and plugin inventory |
| `Shutdown` | Drain and terminate cleanly |

Use the standard gRPC health service on the same Unix socket.

The protocol package MUST contain neutral types only.

It MUST NOT carry Google ADK Go types, ADK-private representations, or copied ADK source.

### Transport authentication

The generic launcher mints one ephemeral Actor CA, host certificate, extension certificate, boot epoch, and message-authentication key for each activation.

Use mTLS on the Unix socket. Certificate SANs bind Actor UID, Revision, container identity, extension ID, and boot epoch.

Mount certificates and the message key through read-only per-container projected secrets. Never place them in the ActorTemplate environment.

Every protobuf message carries protocol, extension, Actor, boot epoch, sequence, and channel digest.

It also carries key ID, algorithm, and HMAC-SHA256 over deterministic protobuf encoding.

The receiver verifies mTLS peer identity, channel binding, HMAC, sequence, deadline, event digest, and boot epoch before processing.

Persist event IDs and terminal decisions in the plugin store for replay-safe duplicate handling. A duplicate event returns the exact original terminal decision.

Reject an old boot epoch except during the explicit `recovery.reconcile` phase for recorded event IDs.

## Negotiation and readiness

Startup order:

1. Substrate starts the kagent and extension containers.
2. The sidecar creates its `0700` socket directory, verifies config, opens state, and serves gRPC.
3. kagent verifies peer identity and the per-launch socket secret.
4. kagent calls `GetExtensionInfo` and checks the manifest against the Revision.
5. kagent sends its generic host inventory to `Negotiate`.
6. The extension selects one protocol major and enabled capability set.
7. kagent calls `Activate` with the immutable Revision and inventory digest.
8. The extension returns accepted or rejected with a stable generic reason code.
9. `/readyz` becomes true only after every required extension and host-native recovery check succeeds.

The host inventory includes every executable path and content sink. It MUST include provider, ADK, kagent, and Substrate versions.

No kagent code decides whether an OpenAPPA policy is covered. The OpenAPPA plugin accepts or rejects the generic inventory.

## Multiple extension ordering

The Revision establishes a total order by `order`, then immutable extension ID.

Phase ownership modes:

| Mode | Behavior |
|---|---|
| `exclusive` | One extension owns uncommitted content and final decision for that phase |
| `transform` | Extension receives only content committed by all earlier exclusive owners |
| `observe_committed` | Extension receives metadata or committed content and cannot modify it |

Only one extension can own an exclusive phase.

The host rejects a phase conflict before Actor readiness.

No runtime plugin, configured callback, or A2A executor plugin can be appended after final ordering validation.

The host keeps one durable generic sequencer per scope.

It assigns the next sequence before callback fan-out and permits one active exclusive phase in that scope.

The first release refuses multiple same-scope function calls in one model response.

Parallel work requires an explicit `child.propose` transition and a new child scope before execution.

Same-scope callback reentry fails immediately. It never waits on its own sequencer lease.

## Event envelope

Every event is immutable:

```protobuf
message ExtensionEvent {
  uint32 protocol_major = 1;
  string extension_id = 2;
  string actor_uid = 3;
  string boot_epoch = 4;
  bytes channel_binding_digest = 5;
  string event_id = 6;
  bytes event_digest = 7;
  string instance_id = 8;
  string revision_id = 9;
  string scope_id = 10;
  optional string parent_scope_id = 11;
  optional string operation_id = 12;
  optional string descriptor_id = 13;
  uint64 sequence = 14;
  string phase = 15;
  google.protobuf.Timestamp deadline = 16;
  Payload payload = 17;
  repeated HostAttestation host_attestations = 18;
  MessageAuthentication authentication = 19;
}

message Payload {
  string codec = 1;
  bytes inline_bytes = 2;
  optional BlobReference blob = 3;
  bytes digest = 4;
}

message HostAttestation {
  AttestationKind kind = 1;
  string issuer = 2;
  bytes credential = 3;
  bytes binding_digest = 4;
  google.protobuf.Timestamp expires_at = 5;
}

enum AttestationKind {
  ATTESTATION_KIND_UNSPECIFIED = 0;
  AUTHENTICATED_PRINCIPAL = 1;
  WORKLOAD_IDENTITY = 2;
  REQUEST_BINDING = 3;
}

message MessageAuthentication {
  string key_id = 1;
  string algorithm = 2;
  bytes mac = 3;
}
```

The host generates IDs and sequence numbers. They carry no plugin semantics.

Large payloads use one sealed temporary blob on a shared memory-backed volume.

Only the host and selected extension can read it.

The host deletes the blob after terminal decision or deadline.

## Generic decisions

```protobuf
message ExtensionDecision {
  uint32 protocol_major = 1;
  string extension_id = 2;
  string actor_uid = 3;
  string boot_epoch = 4;
  bytes channel_binding_digest = 5;
  string event_id = 6;
  bytes event_digest = 7;
  string extension_revision = 8;
  uint64 sequence = 9;
  google.protobuf.Timestamp expires_at = 10;
  MessageAuthentication authentication = 11;
  oneof decision {
    Permit permit = 20;
    Suppress suppress = 21;
    Hold hold = 22;
    Commit commit = 23;
    EventDecision event = 24;
    Fail fail = 25;
    Interaction interaction = 26;
  }
}
```

Decision semantics:

| Decision | Generic host action |
|---|---|
| `permit` | Execute the bound original or replacement payload once |
| `suppress` | Do not execute or publish |
| `hold` | Pause one generic scope without user interaction |
| `interaction` | Publish a neutral versioned interaction request and await `interaction.response` |
| `commit` | Deliver original, replacement, or no result bytes |
| `event` | Drop, replace, or emit one event |
| `fail` | Emit a host-defined content-free failure |

Every decision binds the producing extension, source event, payload digest, expiry, and permitted next phase.

The host rejects mismatched, expired, duplicated, reordered, or unknown decisions.

## Public ADK and kagent integration points

Google ADK Go remains the upstream `v2.2.0` module. Do not use a `replace` directive, vendored or copied source, patches, or `internal` imports.

Each integration below must use a public ADK interface or code owned by the kagent fork.

For a protected workload, an unavailable required boundary rejects `Activate` and keeps the Actor unready.

### Current callback proxy

Wrap current public `plugin.Plugin` callbacks with one generic out-of-process proxy:

- `BeforeRunCallback`
- `AfterRunCallback`
- `OnUserMessageCallback`
- `BeforeAgentCallback`
- `AfterAgentCallback`
- `BeforeModelCallback`
- `AfterModelCallback`
- `OnModelErrorCallback`
- `BeforeToolCallback`
- `AfterToolCallback`
- `OnToolErrorCallback`
- `OnEventCallback`

Install this proxy first in the immutable callback order.

Protected workloads disable any later callback that can observe or change uncommitted content before the proxy decision.

Required phases use public-interface wrappers and kagent-owned barriers below. They do not add callbacks to Google ADK.

### Provider-final catalog

Add a generic gate inside each kagent-owned provider adapter after final name and schema conversion but before request transmission.

It returns the exact provider-visible catalog and reversible logical-name map.

The kagent-owned decoder also returns the provider tool-call ID, mapped logical name, and original JSON argument token bytes.

The host strictly parses and canonicalizes those bytes. It refuses an adapter that exposes only a decoded `map[string]any` or `float64` number path.

Pinned OpenAI code currently decodes arguments through `json.Unmarshal` into a map: [source](https://github.com/kagent-dev/kagent/blob/9e246fd3797457b18fc277680be1629a0f57fce0/go/adk/pkg/models/openai_responses.go#L258-L271).

Reject provider-name collisions before sending the request.

Only a kagent-owned provider adapter, or a public wrapper proven to expose the final outgoing request, qualifies. All other providers are unavailable.

### Tool proposal

kagent constructs the Runner so the generic proxy is the only content-bearing public ADK callback path.

Correlate that callback with raw bytes captured by the kagent-owned provider adapter. Then create an immutable RFC 8785 snapshot and descriptor digest.

If kagent cannot exclude competing callbacks, the callback capability is unavailable.

Refuse tool calls when the provider path did not expose raw arguments before ADK decoded them.

### Model request gate

Place an exclusive `model.propose` gate in each kagent-owned provider adapter after request conversion and before telemetry or network transmission.

A public model wrapper qualifies only when it exposes that same final outgoing request.

For live and realtime sends, wrap the public ADK live-session send interface in kagent-owned code and gate before delegating.

It receives the exact canonical provider request, endpoint, model, tool catalog, history, instructions, memory, admitted tool results, and retry identity.

Only a matching permit can send the request to the provider.

Declare a separate capability for standard generation, live flow, realtime sends, history sends, and embedding requests.

Refuse each path that cannot expose its final request through a kagent-owned adapter or a proven public wrapper.

Provider retry uses the same event ID and exact request digest. A changed request requires a new phase event.

### Model response gate

Use the public `AfterModelCallback` as the standard response gate.

For kagent-owned live and streaming adapters, gate the complete response before kagent dispatches calls or publishes content.

It can replace the complete model response or suppress all contained calls.

Refuse a live or streaming path that bypasses these surfaces.

### Event gate

Implement `Drop`, `Replace`, and `Emit` in kagent wrappers around the public session service and Runner output.

Do not install other content-bearing `OnEventCallback` implementations in a protected workload.

The first release sends every partial assistant event to the exclusive gate and drops it before persistence or yield.

Only one terminal text response can be emitted. Inline data, files, artifacts, citations, thought parts, and multiple terminal responses are refused until their codecs exist.

The terminal event includes a closed authenticated-principal attestation. The host does not interpret the plugin's recipient policy.

### Session persistence gate

Use a kagent-owned session implementation and backing store for user, function-response, model, task, child, and resumed interaction events.

The wrapper receives exact event bytes and stable session lineage.

If the backing store cannot share one transaction with the host outbox, session persistence is unavailable.

### A2A publication gate

Add a kagent-owned barrier before ADK-to-A2A conversion and before stream or task publication.

It receives authenticated caller attestation as generic transport metadata.

### Child lifecycle

Use public Agent, Runner, and session interfaces for child proposal, start, state, terminal return, and parent delivery.

The host preallocates stable child and parent scope IDs.

### Memory lifecycle

Disable stock automatic preload. Supply a kagent-owned implementation through public memory and tool interfaces.

Wrap `SearchMemory` with generic `memory.propose` and `memory.complete` phases.

No memory content enters model instructions before commit.

Expose the embedding-provider request, retrieved content, `load_memory`, `preload_memory`, `save_memory`, and persistent memory write as separate generic operation phases.

Include embedding endpoints and memory stores in the immutable host sink inventory.

### MCP transport

Do not use the upstream opaque MCP client in protected workloads.

Supply a kagent-owned tool implementation with raw JSON-RPC argument injection and terminal response capture.

Disable automatic retry after request bytes can reach the MCP server.

Run MCP discovery during Revision preparation. Pin server identity, transport, tool descriptors, schemas, raw metadata, and discovery generation.

Discovery failure MUST reject preparation. It cannot be logged and skipped.

Controller-originated MCP App tool and resource calls enter through a generic `external.operation` phase before any network request.

The host mints a one-use generic request capability bound to instance, caller, server, scope, operation, expiry, and request ID.

### Remote A2A lifecycle

Add generic phases for prepared Agent Card, outbound request metadata, task state, input-required state, and terminal result.

Strip caller credentials and reserved lineage before the phase call.

Snapshot the exact endpoint, Agent Card digest, headers, task and context IDs, body, retry identity, and transport settings.

Send that complete normalized outbound request through exclusive `remote.request`.

Network transmission requires a matching `permit` bound to the exact request digest.

Accept header or body changes only as replacement payload in the generic decision envelope.

Prepare and pin the Agent Card and endpoint in the Revision. Runtime Card refresh is refused.

Preserve raw `submitted`, `working`, `input_required`, `auth_required`, `completed`, `failed`, `canceled`, `rejected`, and unknown states.

Do not extract content before the exclusive extension commits the terminal state and payload.

## Mechanical execution protocol

The host owns the actual invocation of in-process ADK tools.

For each tool call:

1. Capture provider mapping, dispatch descriptor, and immutable argument bytes.
2. Invoke `tool.propose` on the exclusive extension.
3. Accept only a matching unexpired `permit`.
4. Persist the generic event and decision IDs in host-native delivery state when needed.
5. Invoke `execution.begin` and require plugin acknowledgement.
6. Execute the permitted private payload exactly once.
7. Invoke `tool.complete` with raw output or terminal execution status.
8. Accept only a matching `commit`.
9. Construct the `FunctionResponse` from committed bytes only.

The execution proxy MUST ignore the original mutable ADK map after snapshot creation.

The proxy MUST NOT understand why the extension permitted, replaced, withheld, or failed the operation.

## Result and sink ordering

Raw output cannot reach:

- Later callbacks.
- ADK session persistence.
- Parent or child delivery.
- Model context.
- A2A task or stream publication.
- Memory or UI publication.
- Content logs, traces, metrics, or snapshots.

The exclusive extension receives raw output first.

The host constructs downstream content only from the terminal generic commit.

Before sink publication, write one durable host outbox record keyed by event ID and committed payload digest.

Publish with that stable idempotency key, then mark the outbox row delivered.

Recovery retries only the same outbox event and payload digest.

The kagent-owned session store and its outbox row MUST share one transaction.

A2A, memory, UI, and other external sinks MUST accept the host event ID as an idempotency key.

Refuse a protected sink that lacks idempotent publication. The proposal does not claim exactly-once delivery without sink support.

Protected host construction disables content telemetry before providers, Runner, tools, and log callbacks initialize.

This is an immutable workload property. Extension manifests cannot enable or disable host telemetry.

Configure upstream ADK telemetry through supported public options so it emits no content.

Use content-free constructors for kagent-owned model, tool, Runner, A2A, and MCP instrumentation.

Refuse a component when its content telemetry cannot be disabled without modifying Google ADK.

Remove kagent's standard serialized tool-argument callback from protected workloads.

The host inventory lists every telemetry constructor and exporter with its content-free implementation digest.

At readiness, run a synthetic canary payload through model, tool, session, A2A, memory, and MCP instrumentation.

Search captured records for the canary digest and bytes. Any content-bearing record keeps the Actor unready.

## Dynamic tools and interactions

An extension can contribute a generic tool descriptor through its signed manifest.

kagent does not special-case contributed tool names.

Sidecar-contributed tools execute only through `extension_tool.propose`, `extension_tool.execute`, and `extension_tool.complete` phase envelopes.

No manifest can declare a bespoke executor endpoint.

The OpenAPPA plugin can contribute its control tool and map those generic phases internally to remedies.

For interaction:

1. The plugin returns `interaction` with a versioned neutral presentation document.
2. kagent moves the task to `input_required` without creating a model-visible result.
3. The gateway authenticates the responder and relays opaque response bytes.
4. kagent sends `interaction.response` through `InvokePhase` with the host interaction ID and closed host attestation.
5. The plugin finds private continuation state by host event ID and validates meaning, replay, expiry, and remote-hop state.
6. The host resumes only after a terminal generic decision.

The host does not define approve, decline, cancel, Authority, remedy, or offer fields.

The neutral presentation schema supports title, message, bounded text input, boolean input, and single- or multi-select fields.

The host assigns the interaction ID and binds it to source event, scope, and extension revision.

It also binds caller attestation, schema digest, and expiry.

The host persists only that generic interaction record and submitted field values.

The plugin stores all continuation meaning in its private state keyed by the source event ID.

## OpenAPPA plugin mapping

The OpenAPPA companion translates generic phases into private OpenAPPA runtime events.

| Generic phase | OpenAPPA-side responsibility |
|---|---|
| host inventory activation | Compile `[deployment]`, check path and sink coverage, and attest readiness |
| catalog phases | Validate descriptor and source identity against policy coverage |
| `model.propose` | Check model-provider flow against the current Label and declared provider surface |
| `tool.propose` | Resolve canonical call, Annotator, Membership, requirements, offers, and dispatch |
| `execution.begin` | Persist exact release and execution acknowledgement |
| `tool.complete` | Admit, replace, withhold, or settle terminal outcome |
| child phases | Fork, child identity, return policy, return admission, and merge |
| `remote.request` | Check outbound remote flow and return opaque replacement or permit metadata |
| model and publication events | `assistant.response` emission and admitted event digest |
| `interaction` and `interaction.response` | Remedy, Authority, and other interaction semantics |
| remote phases | Delegation, remote state, conservative contract, and return handling |
| snapshot lifecycle | Commit and validate OpenAPPA plugin state generation |

All OpenAPPA consult kinds, backends, Labels, Values, facts, remedies, effects, and audit records remain inside this component.

### Activation and deployment profile

The plugin compiles one immutable `[deployment]` profile from its private policy configuration.

It requires enforced host execution, harness binding, a concrete starting Label, and explicit confined result points.

It also requires child controls, confined child returns, and no open provider surface.

It verifies the reciprocal host source-to-sink inventory. An empty Engine open-vector set alone is insufficient.

Missing provider, tool, child, memory, MCP, A2A, interaction, telemetry, snapshot, or publication capability rejects `Activate`.

### Exact call and dynamic contract handling

For `tool.propose`, the plugin receives the logical descriptor, provider-final descriptor, reversible name map, source identity, and raw canonical argument bytes.

It performs strict canonical call resolution inside the sidecar.

The plugin handles contracts, Annotators, mandates, Membership evidence, requirement gaps, effects, and remedy plans.

Successful Annotator and Membership evidence remains pinned in the plugin store. Input derivation reselects and re-annotates rewritten calls when required.

The generic `permit` carries only an opaque lease and execution payload digest.

kagent never sees the OpenAPPA dispatch ID or decision meaning.

### Execution and result admission

On `execution.begin`, the plugin persists the OpenAPPA dispatch occurrence and execution acknowledgement before responding.

On `tool.complete`, it maps success, failure, and unknown outcome into current Engine semantics.

Only success commits effects. Failure clears reservations. Conservative uncertainty remains `Indeterminate` and retains effect reservations.

The terminal generic `commit` contains only the admitted original, replacement, or empty payload.

### Children and native attestation

The plugin maps generic child phases to prepared forks, child identities, return policy, return shape, child result, and parent merge.

It supports native `attest-schema` only when the host inventory proves local isolation, structured output, confined return, and exact fork-bound shape.

Remote child results cannot use local `attest-schema` in the first release.

### Remedies and interactions

The sidecar-contributed control tool is an ordinary generic contributed tool to kagent.

Inside the sidecar, it maps to the current remedy families and runtime outcomes.

The plugin owns exact offer binding, authorized and substituted calls, returned Values, decline, no-answer, redispatch, MRTR state, and Authority evidence.

Generic `interaction` and `interaction.response` phases transport presentation and answers. kagent never receives an offer ID or OpenAPPA continuation.

### Assistant response emission

The plugin receives terminal response bytes and the closed authenticated-principal attestation through `model.event` and `a2a.publish` phases.

It maps the principal to its private reader configuration and checks the reserved `assistant.response` emission against the current trajectory Label.

The plugin returns generic `event` with emit, replacement, or drop. kagent does not know the reader or policy reason.

### Replay

The plugin persists OpenAPPA facts and validates them through the Engine before use.

Replay reuses recorded annotations, membership expansions, Authority evidence, Sanitizer derivations, dispatch occurrences, child facts, and emission decisions without external reconsult.

## Plugin state and host state

The OpenAPPA plugin owns one private durable state volume.

It stores:

- Engine facts and runtime projections.
- Policy and deployment identity.
- Call permits, execution acknowledgements, outcomes, and delivery receipts.
- Annotator and Membership evidence.
- Remedy, interaction, and remote delegation state.
- Plugin migrations and snapshot generations.

kagent stores only normal ADK sessions, A2A tasks, and narrow generic delivery records.

Each delivery record contains event ID, event digest, extension revision, phase, host lifecycle state, expiry, and optional delivered-payload digest.

It contains no extension handle, policy decision, continuation, or semantic outcome.

kagent MUST NOT persist opaque plugin handles after their generic delivery lifecycle ends.

## Recovery

At restart:

1. kagent reconstructs host-native session, task, child, and undelivered event inventory.
2. The sidecar opens and migrates its private store.
3. kagent negotiates the same pinned extension revision.
4. kagent sends one `recovery.reconcile` event through `InvokePhase` for each unresolved host event ID.
5. The plugin returns replayed commit, suppress, hold, or fail through the ordinary decision envelope.
6. kagent deduplicates delivery by host event ID.

Crash classification:

- Before plugin acknowledgement of `execution.begin`: no execution started.
- After acknowledgement and before terminal completion: plugin records `Indeterminate` internally.
- After a persisted terminal commit: replay the same generic delivery decision.

The first release accepts this conservative uncertainty window. kagent adds no OpenAPPA-aware execution ledger.

## Snapshots

Snapshot lifecycle is generic and outside ADK callback execution. The user explicitly approved this lifecycle infrastructure.

The controller allocates one monotonically increasing Actor generation and fencing epoch.

Every host-native and extension-owned state write MUST carry the current epoch. A stale Actor or restored process cannot write after a newer generation activates.

Persist a generic host snapshot transaction with these states:

`Active -> Quiescing -> ExtensionsSealed -> HostSealed -> ProviderCommitted -> Complete`.

Sequence:

1. Reject new roots and tool proposals.
2. Drain or cancel active host operations.
3. Call `Quiesce` on every required extension in deterministic order.
4. Receive signed extension generation metadata and file digests.
5. Checkpoint host-native stores.
6. Ask Substrate to create one encrypted snapshot generation.
7. Bind provider snapshot identity, key version, host files, extension files, and completion marker.
8. Call `ValidateRestore` before any restored Actor becomes ready.

The OpenAPPA state volume is mounted only in the OpenAPPA sidecar.

Each extension manifest binds generation, fencing epoch, state schema, file digests, WAL state, policy-independent extension revision, and encryption-key version.

The host manifest binds its session and delivery files plus every signed extension manifest.

Publish `Complete` only after the provider confirms the snapshot identity and all file digests.

Recovery rules:

- Before `ExtensionsSealed`: abort the snapshot and resume the old generation.
- After `ExtensionsSealed` but before `ProviderCommitted`: keep the Actor quiesced and retry or abort through each extension.
- After `ProviderCommitted` but before `Complete`: verify provider identity and digests, then finish the same transaction.
- On any digest, epoch, signature, or state-schema mismatch: reject restore and remain unready.

Use `ReadWriteOncePod` plus the durable epoch. Volume access mode alone is not a fencing mechanism.

## Readiness and failure behavior

`/readyz` returns success only when:

- Every required sidecar container is healthy.
- Artifact, manifest, protocol, and workload identities match the Revision.
- Capability and phase negotiation succeeded.
- Every exclusive phase has one owner.
- The extension accepted the full host inventory.
- Host and plugin recovery completed.
- Egress, secret, volume, and snapshot attestations passed.

Failure rules:

| Failure point | Host behavior |
|---|---|
| Before permit | Suppress execution and return content-free failure |
| After execution acknowledgement | Withhold result and freeze scope pending recovery |
| Before event publication | Drop or replace with content-free failure |
| Required extension unhealthy | Mark Actor unready and reject new work |
| Protocol or manifest mismatch | Refuse activation |
| Unknown or expired handle | Refuse the operation |

Required extension calls fail closed. Optional extensions cannot claim exclusive phases.

## Egress and secrets

The generic extension declaration lists destinations and secret references.

The kagent controller validates generic shape and includes hashes in the Revision.

Substrate MUST enforce per-container egress through an authenticated gateway. Current destination metadata alone is insufficient.

Install Actor-wide default-deny CNI egress. Permit only the cluster DNS resolver and authenticated egress gateway addresses.

Block direct external TCP, UDP, node, control-plane, and cloud metadata-service routes.

Each container authenticates to the gateway with its workload identity. The gateway selects the allowlist for that exact Actor, Revision, and container.

The gateway resolves external DNS and enforces scheme, host, port, and TLS or SPIFFE identity.

It also enforces IP ranges, redirects, and DNS rebinding rules.

The Actor MUST remain unready when CNI isolation, gateway routing, DNS policy, or workload identity cannot be installed and attested.

Add projected Secret and ConfigMap volume support so secret bytes do not appear as literal ActorTemplate environment values.

The sidecar receives only declared mounts, secrets, and egress.

## Security and trust boundary

Require signed OCI image and manifest artifacts, allowed signer identities, SBOM, and build provenance.

Publish the extension manifest as OCI media type `application/vnd.kagent.extension.manifest.v1+json`.

Publish SPDX JSON SBOM and in-toto SLSA provenance as OCI referrers to the same image subject digest.

Publish a Sigstore signature envelope for the complete artifact set.

It binds image, manifest, SBOM, provenance, signer identity, and Rekor inclusion proof.

The generic trust policy pins allowed registry, issuer, subject identity, and minimum provenance level.

The controller verifies all referrer subject links and signatures before Revision creation. Cluster admission verifies the same Revision attestation before Actor launch.

Admission MUST reject tag-only images, missing or mismatched referrers, unsigned manifests, disallowed registries, unknown signers, and Revision overrides.

Restrict Harness, AgentTemplate, AgentInstance, Revision, ActorTemplate, Secret, and direct workload mutation through RBAC and admission policy.

Run the sidecar as a dedicated non-root UID with read-only root filesystem, `allowPrivilegeEscalation: false`, dropped Linux capabilities, and RuntimeDefault seccomp.

Mount its private state with single-writer fencing. Use `ReadWriteOncePod` or one external transactional writer lease per plugin revision.

The kagent process is part of the trusted computing base. In-process tool code shares that process and can reach the private client socket as the host identity.

The sidecar boundary isolates OpenAPPA memory and state from ordinary accidental faults. It does not sandbox malicious code already executing inside kagent.

Use one per-launch secret and workload identity on the Unix gRPC channel. Bind every event to boot epoch, instance, Revision, extension, sequence, and digest.

RPC deadlines are mandatory. Disable hedging. Retry an extension RPC only with the same idempotent event ID.

Never retry a tool transport after request bytes can have reached the tool.

The generic scope lock is non-reentrant. A nested operation must create a declared child scope or fail before waiting.

Bound callback queues, child concurrency, interaction lifetime, and extension restart backoff.

## Plugin upgrades

Pin the extension artifact, manifest, protocol, and state schema to every live scope.

Before first dispatch, persist an immutable mapping from root, task, context, child, and interaction scope to ActorTemplate and extension Revision.

Every resume, remote response, interaction response, capability, and late event routes through that mapping.

Controller lifecycle is `Active -> Draining -> Drained -> Suspended`.

Upgrade sequence:

1. Prepare and activate the new sidecar revision against the new host inventory.
2. Atomically stop old-root admission and route new roots to the new Revision.
3. Keep existing scopes on the old sidecar revision.
4. Keep the old workload ready for its assigned scopes only.
5. Drain old scopes until terminal or deadline.
6. Record every forced remainder as generic uncertain termination for plugin reconciliation.
7. Snapshot and suspend the old instance only after pending work reaches zero or reconciliation completes.

Rollback is another atomic new-root routing transition. It never moves a live scope between revisions.

No between-turn or immediate hot swap exists in the first release.

## Generic kagent PR sequence

| PR | Change |
|---|---|
| 1 | Add generic Harness extension API, manifests, Revision data, and signature policy |
| 2 | Render sidecar containers, projected config and secrets, socket and state volumes, egress, and readiness |
| 3 | Add neutral protobuf protocol, negotiation, health, and generic extension host |
| 4 | Gate provider-final requests and catalogs in kagent-owned model adapters |
| 5 | Install the first public tool callback proxy and kagent-owned execution wrappers |
| 6 | Gate model responses through public callbacks and kagent-owned output wrappers |
| 7 | Wrap the public session service and kagent A2A publication code |
| 8 | Wrap public Agent, Runner, memory, remote, and interaction surfaces |
| 9 | Add a kagent-owned raw MCP transport with no-retry semantics |
| 10 | Configure upstream telemetry and add kagent readiness, snapshot, recovery, and drain lifecycle |

Each PR is generic. Test fixtures MUST use neutral fake extensions and contain no OpenAPPA import or name in production packages.

No PR changes Google ADK source, copies it, replaces its module, vendors it, or imports one of its `internal` packages.

## OpenAPPA plugin sequence

| PR slice | Change |
|---|---|
| 1 | Neutral protocol binding, sidecar handshake, inventory validation, and health |
| 2 | Tool proposal, permit, begin, complete, commit, and conservative recovery |
| 3 | Event, session, assistant response, and publication gates |
| 4 | Child, task, remote, memory, and MCP mappings |
| 5 | Dynamic remedy tool, interaction relay, and durable continuation |
| 6 | Plugin state, snapshots, migrations, signed manifest, image, SBOM, and provenance |

## Verification matrix

### Generic host unit tests

- Manifest signature, digest, protocol range, and capability validation.
- Deterministic ordering and exclusive-phase conflicts.
- Socket identity and launch-secret validation.
- Event canonicalization, digest, deadline, and sequence checks.
- Permit, commit, event, hold, interaction, and fail decision validation.
- Unknown, expired, replayed, cross-scope, and reordered handles.
- Payload inline and sealed-blob limits.
- Required sidecar health and readiness transitions.
- Pin-and-drain upgrade routing.
- Forbidden dependency and identifier checks.

### Public ADK and kagent integration tests

- CI resolves `google.golang.org/adk/v2` from a clean module cache and asserts the pinned version and checksum.
- CI rejects `replace` in `go.mod` or `go.work`, a `vendor/` tree, copied ADK source, any ADK source patch, and any ADK `internal` import in production packages.

- Provider-final catalog and name-collision rejection.
- Pre-plugin argument ownership and mutation attempts.
- Ordinary tool allow, replace, suppress, error, panic, and timeout.
- No duplicate execution after transport uncertainty.
- Standard and live model response gates.
- Event drop before plugin observation, session append, and yield.
- Forged inbound `FunctionResponse` rejection.
- Local chat, single-turn, task, and terminal child return.
- Automatic memory before-search and after-result gates.
- Raw MCP arguments, response, reconnect, and no-retry behavior.
- Remote immutable Card, credential stripping, task states, and terminal result.
- A2A publication and authenticated interaction relay.
- Content telemetry disabled before payload creation.

### Sidecar lifecycle tests

- Container and protocol readiness ordering.
- Required sidecar crash before and during operations.
- Bounded restart and re-registration.
- State volume isolation from kagent.
- Egress and secret mount enforcement.
- Quiesce, encrypted snapshot, partial snapshot rejection, and restore validation.
- Old and new plugin revisions draining concurrently without scope migration.

### OpenAPPA plugin tests

- Host inventory acceptance and every missing-capability refusal.
- Deployment profile and sink coverage validation.
- Annotator, Membership, Authority, Sanitizer, and native attestation paths.
- Remedy families and every runtime outcome.
- Result, child-return, and assistant response admission.
- Human interaction accept, decline, cancel, expiry, replay, and remote hops.
- Crash before permit, before begin acknowledgement, during execution, after result, and after commit.
- Replay of exact terminal decisions and `Indeterminate` effect handling.

### End-to-end GCP acceptance

- Build and deploy the generic kagent host and OpenAPPA sidecar as separate images.
- Verify dynamic manifest installation without recompiling kagent.
- Run ordinary, MCP, memory, local child, remote child, HITL, final response, and migration scenarios.
- Inject plugin, host, network, tool, model, and snapshot failures.
- Inspect logs, traces, session state, plugin state, A2A output, and snapshots for prohibited raw content.
- Repeat all crash and restart windows until deterministic assertions pass.
- Manually verify desktop and API interaction flows.

The PoC is complete only when every required capability passes and no partial-capability mode reports ready.
