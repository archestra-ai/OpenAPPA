---
title: kagent integration
nav_title: kagent
description: Proposal for a dynamically supplied OpenAPPA ADK extension in kagent.
---

:::proposal
name: kagent integration
date: 2026-08-27
:::

This proposal adds a generic dynamic ADK extension host to the kagent Go fork.

OpenAPPA ships as a separate OCI companion container. The kagent fork contains no OpenAPPA policy, Label, trajectory, remedy, consult, runtime, or persistence logic.

The combined protected instance enforces four contracts:

1. The host executes only the immutable payload permitted by the required extension.
2. Raw tool and child results remain withheld until the extension commits a delivery payload.
3. Model, session, A2A, memory, UI, log, telemetry, and storage publication cross exclusive extension gates.
4. The instance remains unready when its generic host inventory cannot satisfy every required extension capability.

This proposal uses [kagent commit `9e246fd37`](https://github.com/kagent-dev/kagent/commit/9e246fd3797457b18fc277680be1629a0f57fce0) as its source baseline.

It also uses Google ADK Go 2.2.0 and Substrate 0.0.20.

OpenAPPA policy semantics remain in [How it works](/how-it-works) and [Policy contracts](/contracts).

## Keep the fork generic

The kagent fork implements only generic extension infrastructure:

- Dynamic extension configuration and artifact verification.
- Version and capability negotiation.
- Immutable ADK event snapshots.
- New generic ADK callback and barrier points.
- Generic execution, event, interaction, and lifecycle decisions.
- Generic sidecar readiness, egress, volume, snapshot, and drain handling.

The fork MUST NOT import an OpenAPPA crate, Go package, protobuf package, policy schema, wire enum, or generated type.

The generic protocol MUST NOT contain OpenAPPA domain terms.

Examples include Label, Trajectory, Authority, Annotator, Sanitizer, Membership Resolver, remedy plan, ReaderId, and effect.

The OpenAPPA companion owns:

- Policy loading and `[deployment]` validation.
- OpenAPPA Engine and runtime calls.
- Facts, call correlation, consult pins, offers, continuations, and recovery.
- Annotator, Membership Resolver, Authority, and Sanitizer implementations.
- OpenAPPA-specific A2A delegation and human-review meaning.
- Plugin-side durable state and schema migrations.

kagent sees only generic ADK payloads, phase names, digests, deadlines, decisions, and opaque handles.

## Use an OCI companion container

The selected process model is a digest-pinned OCI companion container in the same Substrate Actor.

Substrate already supports [multiple containers with independent readiness probes](https://github.com/kagent-dev/substrate/blob/v0.0.20/pkg/api/v1alpha1/actortemplate_types.go#L243-L356). Pinned kagent currently emits [one container](https://github.com/kagent-dev/kagent/blob/9e246fd3797457b18fc277680be1629a0f57fce0/go/core/v2/substrate/actor_template.go#L32-L91).

The baseline lacks shared memory sockets, projected config and secrets, per-container identity, filesystem controls, seccomp, and enforced egress.

These are required generic Substrate additions. Extension activation fails when any declared primitive is unavailable.

The generic fork adds extension containers without adding an OpenAPPA-specific sidecar role.

The kagent process and extension communicate over gRPC on a private Unix-domain socket. A shared memory-backed socket volume carries no durable state.

Substrate mints per-activation mTLS identities, a boot epoch, and a message-authentication key.

Every event and decision binds the Actor UID, extension ID, protocol major, boot epoch, sequence, channel digest, deadline, and HMAC.

The extension receives a separate durable state volume. The kagent container cannot mount that volume.

The extension receives opaque configuration and secret mounts. kagent verifies source references and digests but does not parse their content.

The extension image runs as a dedicated UID with a read-only root filesystem and no Linux capabilities.

It also has bounded resources and deny-by-default egress.

Actor-wide CNI policy permits only cluster DNS and an authenticated egress gateway. The gateway enforces per-container destination, TLS identity, redirect, and DNS-rebinding rules.

The kagent process and its in-process tools remain in the trusted computing base. The sidecar isolates OpenAPPA memory and state but cannot sandbox malicious code already running as kagent.

## Follow proven plugin patterns

The protocol borrows established mechanisms rather than using Go shared-library loading.

| Source | Adopted mechanism |
|---|---|
| [HashiCorp go-plugin](https://github.com/hashicorp/go-plugin) | Out-of-process RPC, handshake, protocol-major negotiation, health, process isolation, and explicit lifecycle |
| [Terraform provider protocol](https://developer.hashicorp.com/terraform/plugin/terraform-plugin-protocol) | Artifact version separate from wire compatibility and strict protocol negotiation |
| [Kubernetes device plugins](https://v1-32.docs.kubernetes.io/docs/concepts/extend-kubernetes/compute-storage-net/device-plugins/) | Serve before registration, Unix-socket gRPC, capability discovery, health, and re-registration |
| [VS Code extension manifests](https://code.visualstudio.com/api/references/extension-manifest) | Declarative contribution points, compatibility ranges, activation, and reviewable requested capabilities |
| [Envoy xDS](https://www.envoyproxy.io/docs/envoy/latest/api-docs/xds_protocol) | Versioned configuration with explicit accept or reject behavior |

The design does not use the Go [`plugin`](https://pkg.go.dev/plugin) package. Its toolchain coupling, platform limits, race-detector limitations, shared address space, and unload restrictions conflict with independently shipped extensions.

## Configure extensions dynamically

A Harness can declare several ordered dynamic extensions. Each declaration is generic:

```yaml
extensions:
  - name: policy-gate
    image: ghcr.io/archestra-ai/openappa-adk-plugin@sha256:<digest>
    manifestDigest: sha256:<digest>
    protocol: kagent.extension.v1
    required: true
    failureMode: closed
    order: 10
    socket: /run/kagent/extensions/policy-gate.sock
    config:
      configMapRef:
        name: customer-support-policy-v1
    secrets:
      - secretRef:
          name: openappa-plugin-secrets-v1
    state:
      durableDir: openappa-plugin-state
    egress:
      - api.policy-provider.example:443
    readiness:
      path: /readyz
      port: 8082
```

The generic compiler verifies:

- OCI digest and signature policy.
- Manifest digest and protocol range.
- Canonical opaque configuration digest.
- Secret and volume references without reading plugin semantics.
- Declared capabilities, exclusive phases, deadlines, limits, and failure mode.
- Socket, state, egress, readiness, and resource declarations.

The controller resolves opaque ConfigMap and Secret bytes only to hash and copy them.

It mounts immutable Revision-owned copies and stores no secret bytes in the Revision. Any policy, credential, CA, or secret change creates a new Revision.

The immutable Revision includes these generic values. A plugin revision never changes inside a live scope.

## Negotiate a neutral protocol

The sidecar starts before registration and serves the private socket.

The host performs these steps:

1. Verify the sidecar workload identity and per-launch socket secret.
2. Call `GetExtensionInfo` for plugin ID, artifact version, protocol range, state schema, and manifest digest.
3. Send the complete generic host capability and sink inventory.
4. Select one protocol major and capability set.
5. Require the extension to accept the inventory digest and return ready.
6. Enable the extension only after both container readiness and protocol readiness succeed.

The inventory describes mechanics, not OpenAPPA policy:

- ADK and provider versions.
- Available callback phases and their ordering.
- Tool, child, remote, memory, MCP, interaction, event, session, and publication paths.
- Provider-final descriptor and name-mapping support.
- Immutable JSON and raw transport codecs.
- Content-bearing sinks and telemetry surfaces.
- Snapshot, recovery, and drain capabilities.

The OpenAPPA sidecar validates its own policy against that inventory.

kagent treats the signed ready response as a generic required-extension attestation.

## Add generic ADK extension points

Current ADK callbacks cover only part of the required lifecycle.

| Boundary | Baseline status | Generic fork addition |
|---|---|---|
| Ordinary `BeforeTool`, `AfterTool`, and tool error | Existing plugin callbacks | Dynamic out-of-process proxy and deterministic ordering |
| User message before persistence | Existing `OnUserMessageCallback` | Generic pre-persistence phase keyed by host event ID |
| Model response before normal function dispatch | Existing `AfterModelCallback` on the standard path | One shared gate for standard and live paths |
| Final model request before provider transmission | Missing | Exclusive `model.propose` gate for standard, live, realtime, history, and embedding paths |
| Provider-final tool catalog | Missing | Final descriptor and reversible provider-name callback |
| Before every plugin sees tool arguments | Missing | Exclusive pre-plugin dispatch gate |
| Immutable argument ownership | Missing | Host-owned RFC 8785 snapshot and private execution payload |
| Event drop before session persistence and yield | Missing | Exclusive `Drop`, `Replace`, or `Emit` event gate |
| Child and task terminal return | Missing | Child terminal and parent-return gates |
| Final A2A publication | Missing | Replaceable pre-publication gate |
| Automatic memory preload | Outside tool callbacks | Synthetic memory operation lifecycle |
| Raw MCP transport and no-retry dispatch | Outside plugin callbacks | Generic transport interceptor and execution proxy |
| Remote A2A request before network transmission | Existing custom tool, no gate | Exclusive `remote.request` snapshot and permit phase |
| Snapshot, readiness, egress, and drain | Outside ADK | Generic extension lifecycle interface |

The user approved the generic lifecycle interface. Strict callback-only integration would remove several required guarantees.

Those guarantees include snapshots, readiness, raw MCP, telemetry, and publication.

No OpenAPPA payload or decision uses a direct kagent API. Enforcement data crosses only the generic ADK extension protocol.

The only non-ADK messages are generic sidecar lifecycle, readiness, workload identity, opaque configuration, volume, egress, and snapshot metadata.

## Keep extension ordering deterministic

The Revision defines one total extension order.

An extension manifest declares each phase as `exclusive`, `transform`, `observe_committed`, or unused.

Only one extension can own an exclusive phase. Revision preparation rejects an ownership conflict.

The OpenAPPA plugin requires exclusive ownership of all uncommitted content phases.

Other extensions can observe metadata or already committed content only after that exclusive gate.

No runtime plugin can be appended after manifest verification.

## Use immutable event envelopes

Every host event has a generic immutable envelope:

```json
{
  "protocol": "1.0",
  "event_id": "opaque-host-id",
  "event_digest": "sha256:...",
  "instance_id": "opaque",
  "scope_id": "opaque",
  "parent_scope_id": null,
  "operation_id": "opaque",
  "descriptor_id": "opaque",
  "sequence": 42,
  "phase": "tool.propose",
  "deadline": "2026-09-01T00:00:00Z",
  "payload": {
    "codec": "rfc8785-json",
    "bytes": "<bounded bytes or blob reference>",
    "digest": "sha256:..."
  }
}
```

The host IDs are generic and unguessable. They bind instance, Revision, scope, phase, descriptor, operation, expiry, and allowed use.

The sidecar can return an opaque lease for one immediate mechanical next phase.

The host never parses a lease or persists it beyond that delivery lifecycle.

The sidecar resolves held interactions and recovery state from its private store by host event ID.

## Use generic decisions

The sidecar returns only generic host decisions:

| Decision | Host behavior |
|---|---|
| `permit` | Execute the original or replacement immutable payload once |
| `suppress` | Execute or publish nothing |
| `hold` | Pause one generic scope without requesting user input |
| `interaction` | Publish a neutral interaction request and wait for a generic response event |
| `commit` | Deliver original, replacement, or no result bytes |
| `event` | Drop, replace, or emit one ADK, session, or A2A event |
| `fail` | Emit a host-defined content-free failure |

A decision binds the event digest, extension revision, deadline, and permitted next phase.

The host executes no replacement that lacks a matching permit. It publishes no content that lacks a matching commit or event decision.

## Gate one tool lifecycle mechanically

```text
ADK proposes a tool call
  -> host captures final descriptor and immutable arguments
  -> sidecar: tool.propose
  <- sidecar: permit(exact payload digest, opaque lease)
  -> sidecar: execution.begin
  <- sidecar: acknowledged
  -> host invokes the tool once with the permitted private payload
  -> sidecar: tool.complete(raw result or terminal status)
  <- sidecar: commit(original, replacement, or no bytes)
  -> host constructs the FunctionResponse from committed bytes only
```

The extension sees the provider-final descriptor, actual source identity, exact canonical arguments, and generic execution context.

The host blocks name collisions, descriptor drift, mutable argument reuse, callback bypass, and automatic transport retry before plugin policy runs.

## Gate the model provider request

After all request processors and model callbacks, the host snapshots the exact provider request.

The snapshot includes endpoint, model, catalog, history, instructions, memory, and admitted results.

It sends `model.propose` before telemetry or network transmission. Only a matching permit can send that exact request.

The same gate covers standard generation, live flow, realtime sends, history sends, embeddings, and every provider adapter.

## Gate every result and response sink

The generic event gate runs before any plugin callback can observe uncommitted event content.

It runs before ADK session append, runner yield, parent return, task state, and A2A conversion.

It also runs before A2A publication, memory insertion, UI publication, content logs, trace attributes, and background storage.

The gate supports `Drop`, `Replace`, and `Emit`. No callback runs between the final event decision and the protected sink.

The first release drops every partial assistant event before persistence and yield.

It emits one terminal text response only. It refuses files, data parts, artifacts, citations, thought content, and multiple terminal responses.

The OpenAPPA plugin maps these generic events to its own admission and `assistant.response` logic internally.

Protected host construction always disables content-bearing telemetry and argument logging before Runner creation.

This immutable kagent workload property does not depend on an extension manifest or decision.

Readiness sends a canary through every model, tool, session, A2A, memory, MCP, and exporter path. Any captured canary content keeps the Actor unready.

## Cover children, remote agents, and interactions

The host exposes generic child phases:

- `child.propose`
- `child.started`
- `child.terminal`
- `remote_child.state`
- `remote_child.terminal`

A child starts only after a permit bound to its parent scope and prepared child descriptor.

No child result reaches a parent before a matching commit.

The remote client extension point exposes prepared Agent Card identity, outgoing headers, task state, and terminal content.

The host strips caller credentials and reserved lineage headers.

It sends the complete normalized outbound request through `remote.request` and accepts header or body replacement only through the generic decision envelope.

For human interaction, `interaction` carries a versioned neutral presentation document and host interaction ID.

kagent moves the task to `input_required`, authenticates the responder, and emits `interaction.response` through the ordinary extension phase API.

It does not interpret approve, decline, cancel, Authority, offer, or remedy meaning.

The OpenAPPA plugin owns response meaning, replay protection, expiry, remote-hop state, and resume decisions.

## Keep OpenAPPA semantics inside the sidecar

The OpenAPPA sidecar maps generic phases to current Engine and runtime behavior.

It alone compiles the concrete `[deployment]` profile, validates the host inventory, and refuses readiness when coverage is incomplete.

It alone implements:

- Canonical call and result correlation.
- Annotator routing, mandates, pins, and rewrite semantics.
- Membership resolution and operation evidence.
- Authority and Sanitizer consults.
- `attest-schema` child-return handling.
- Remedy plans and runtime outcome translation.
- Label, Value, effect, child, offer, and emission facts.
- OpenAPPA audit and recovery state.

The kagent fork does not know these concepts.

## Keep state ownership separate

kagent keeps its ordinary ADK session and A2A task stores.

Its only extension record contains host event ID, digest, extension revision, phase, delivery state, expiry, and optional delivered-payload digest.

The OpenAPPA sidecar keeps all OpenAPPA state in its private volume.

This state includes facts, leases, journals, consult pins, remedies, interactions, delegations, and delivery receipts.

kagent stores no OpenAPPA journal, token, fact, offer, Label, decision, or runtime database.

Generic host event IDs support delivery deduplication. They do not encode plugin meaning.

Before a snapshot, the generic lifecycle host stops new work and asks every required extension to quiesce.

The sidecar returns a signed state-generation manifest. Substrate then checkpoints host-native and extension-owned volumes and attests the encrypted snapshot generation.

A durable fencing epoch accompanies every state write. The host records one atomic snapshot transaction from quiescing through provider completion.

Restore remains blocked until every required extension validates its own generation and accepts the host inventory.

## Classify uncertain crashes conservatively

The selected first-release recovery rule is conservative.

A crash before the sidecar acknowledges `execution.begin` means the host did not execute the tool.

A crash after that acknowledgement and before a terminal result is `Indeterminate` to the OpenAPPA sidecar.

The kagent host does not keep an OpenAPPA-aware execution journal to narrow this window.

The host emits `recovery.reconcile` through the ordinary extension phase API.

The affected generic scope remains frozen until the sidecar returns `suppress`, `hold`, replayed `commit`, or terminal `fail`.

## Pin and drain plugin upgrades

One plugin artifact and protocol selection remain pinned for each live scope.

The controller durably maps every root, task, context, child, and interaction scope to its ActorTemplate and extension Revision before dispatch.

New roots can use a new plugin revision only after it accepts the new host inventory and reports ready.

Existing scopes drain on the old sidecar revision. The host never hot-swaps a plugin between turns or during a tool lifecycle.

The old workload rejects new roots but remains available for its assigned scopes until terminal or deadline.

The plugin owns any state migration. An incompatible revision requires instance replacement and drain.

## Install the generic host and OpenAPPA plugin

Installation has two independently versioned artifacts:

1. A kagent fork with the generic dynamic extension host and generic ADK/lifecycle points.
2. The OpenAPPA ADK plugin companion image and its signed manifest.

Create immutable policy configuration for the sidecar:

```sh
kubectl -n kagent create configmap customer-support-policy-v1 \
  --from-file=appa.toml=./appa.toml \
  --dry-run=client -o yaml | kubectl apply -f -
kubectl -n kagent patch configmap customer-support-policy-v1 \
  --type=merge -p '{"immutable":true}'
```

The generic Harness references the extension image and opaque configuration. The kagent controller never parses `appa.toml`.

The OpenAPPA plugin reports unready until its policy, state, runtime, host inventory, and required generic phases validate.

Create a new `AgentInstance` from the prepared Harness. Existing instances cannot change their pinned extension set.

## Architecture

```text
+----------------------------- Kubernetes / Substrate Actor ----------------------+
|                                                                                  |
|  kagent controller -> immutable Revision -> ActorTemplate                        |
|                                |                                                 |
|                 +--------------+----------------+                                |
|                 |                               |                                |
|                 v                               v                                |
|  +-----------------------------+  Unix gRPC  +--------------------------------+  |
|  | kagent-go-adk               |<----------->| dynamic ADK extension sidecar |  |
|  |                             | private UDS |                                |  |
|  | generic extension host      |             | OpenAPPA plugin server        |  |
|  | generic ADK barriers        |             | appa-runtime + Engine         |  |
|  | generic execution proxy     |             | policy and consults           |  |
|  | sessions.db / A2A state     |             | private plugin state          |  |
|  +-------------+---------------+             +----------------+---------------+  |
|                |                                                  |              |
|                v                                                  v              |
|       model / tools / MCP / A2A                         declared external endpoints|
|                                                                                  |
+----------------------------------------------------------------------------------+
```

The Unix socket and generic event protocol are the only enforcement-data interface between kagent and the OpenAPPA component.

## Refuse incomplete coverage

The protected instance remains unready when any required phase is missing, shared, reordered, bypassable, or unsupported.

The first release refuses:

- Provider-native tools without host dispatch and result boundaries.
- Stock MCP transport when raw argument injection and no-retry execution are unavailable.
- Automatic memory preload without the synthetic lifecycle extension.
- Dynamic tool catalogs that change inside a prepared Revision.
- Runtime plugins or callbacks appended after manifest verification.
- Content telemetry that cannot be disabled before payload creation.
- Remote A2A paths without immutable Card, header replacement, state preservation, and terminal gates.
- Streaming assistant content before the exclusive publication gate.
- Hot plugin swaps and mixed plugin revisions inside one scope.
- Any partial capability mode for the required OpenAPPA plugin.

## Move an existing agent

An existing `AgentInstance` cannot add a new dynamic extension set.

Create a replacement protected instance and route new roots to it.

Keep existing task and context IDs on the old instance until a fixed drain deadline.

At the deadline, cancel remaining old work and suspend the old instance.

The replacement starts new ADK session and OpenAPPA plugin state. It imports neither the old transcript nor old plugin state unless the plugin explicitly validates a migration.

Rollback resumes the old instance and restores routing. It never merges state between plugin revisions.

## Implementation plan

The [kagent implementation plan](../../../integrations/kagent/IMPLEMENTATION.md) defines the generic protocol, source ownership, and new ADK extension points.

It also defines sidecar lifecycle, OpenAPPA mapping, and the verification matrix.
