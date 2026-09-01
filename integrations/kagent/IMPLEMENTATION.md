# kagent dynamic adapter implementation plan

Source baseline:

- kagent commit `9e246fd3797457b18fc277680be1629a0f57fce0`
- Google ADK Go `v2.2.0` at `b264039aaec43baedc123e5b9a0cf87681d0bbca`
- Substrate `v0.0.20`
- OpenAPPA `origin/main` at `d87d6f822fdde2968d607b05f2be4c6140b67342`

The reader-facing proposal is at [openappa.com/kagent](https://www.openappa.com/kagent).

## Architecture decision

`appa-adapter-kagent` is the OpenAPPA kagent adapter. It maps generic kagent harness events to the existing `appa-runtime` `/hook` HTTP interface.

The adapter follows the `appa-adapter-claude-code` boundary. It depends on `appa-runtime-api` for hook wire types and codecs. It does not link `appa-runtime`, call the Engine, own policy, or open `appa.db`.

`appa-runtime` is a separate logical process. It owns policy loading, the Engine, consults, remedy plans, trajectory state, recovery semantics, and `appa.db`. The adapter sends a bound `/hook` request and maps its bound response to the generic kagent decision wire.

The adapter fails closed when `APPA_RUNTIME_URL` is unavailable, unauthenticated, expired, mismatched, or unreachable. A runtime response without the required identity and revision binding fails closed.

Google ADK Go remains an unmodified upstream dependency. The implementation has no fork, vendored or copied source, patch, module replacement, or `internal` import.

## Ownership and forbidden dependencies

| Owner | Responsibilities |
|---|---|
| kagent fork | Neutral extension API, manifest and Revision validation, sidecar rendering, public-ADK wrappers, payload snapshots, generic decisions, host sink barriers, and host-native session and A2A state |
| `appa-adapter-kagent` | Generic protocol negotiation, mTLS and HMAC channel state, event replay and delivery ledger, lifecycle relay, and kagent wire to `/hook` codec |
| `appa-runtime` | Runtime API, policy, Engine, consults, remedies, trajectory state, recovery, audit, policy configuration, and `appa.db` |

`appa-adapter-kagent` has no OpenAPPA policy parser, `[deployment]` validation, Engine dependency, Engine call, consult backend, remedy planner, trajectory store, recovery classifier, dynamic policy tool, policy schema migration, or `appa.db` path.

The adapter ledger contains only generic protocol values: event ID and digest, actor and extension IDs, adapter artifact digest, runtime identity, policy revision, boot epoch, channel digest, sequence, expiry, delivery status, and response digest. It contains no Label, Value, fact, offer, continuation, dispatch identifier, policy decision, consult evidence, or runtime database record.

The kagent fork has no OpenAPPA named package, field, enum, callback, table, error code, tool route, policy type, or database migration. CI scans the generic-host packages for the maintained forbidden set.

## Deployment profiles

### Quickstart profile

Quickstart has one digest-pinned OpenAPPA companion container and two processes:

```text
+---------------------------- OpenAPPA companion container ----------------------------+
| appa-adapter-kagent                                                          :8082   |
| private kagent UDS gRPC -> neutral event -> /hook request                            |
|                                      | APPA_RUNTIME_URL=http://127.0.0.1:8787         |
| appa runtime --adapter kagent        | policy, Engine, consults, remedies, appa.db   |
| HTTP listener: 127.0.0.1:8787 only   |                                                |
+---------------------------------------------------------------------------------------+
```

The runtime port has loopback binding only. The container has no service, ingress, or network-policy exception for this port. Only `appa-runtime` mounts the policy, credentials, and `appa.db` volume. The adapter mounts only its generic delivery ledger and connection material.

### Remote configuration profile

Remote configuration uses the same adapter artifact. `APPA_RUNTIME_URL` identifies an authenticated HTTPS gateway or runtime instance.

```text
kagent host -> private UDS -> appa-adapter-kagent -> mTLS HTTPS -> bound runtime gateway -> appa-runtime
```

The adapter trusts a configured CA and remote workload identity. The remote gateway authenticates the adapter workload identity. It forwards only to the runtime identity bound by the immutable Revision. The runtime verifies Actor UID, extension ID, adapter artifact digest, runtime identity, policy revision, boot epoch, sequence, event digest, and request channel digest.

The runtime response binds the same values and its accepted policy revision. Redirects, certificate mismatch, identity mismatch, endpoint mismatch, policy revision mismatch, unknown runtime identity, and expired bindings fail closed. Remote configuration gives the adapter no runtime policy mount or `appa.db` volume.

## Repository deliverables

| Owner | Component |
|---|---|
| OpenAPPA repository | `appa-adapter-kagent` crate and binary |
| OpenAPPA repository | `appa-adapter-kagent-protocol` crate for neutral kagent wire bindings |
| OpenAPPA repository | `appa-runtime-api` hook types and codecs shared by the runtime and adapters |
| OpenAPPA repository | `appa-runtime` `/hook` binding validation, runtime mapping, policy, Engine, and `appa.db` ownership |
| OCI packaging | `appa-adapter-kagent` image, quickstart companion image, manifests, SBOM, and provenance |
| kagent private fork | Generic host and public-ADK integration changes |

The adapter binary is `appa-adapter-kagent`. The quickstart runtime process is `appa runtime --adapter kagent`. The adapter image is `ghcr.io/archestra-ai/appa-adapter-kagent@sha256:<digest>`. The quickstart companion image contains this adapter and the runtime binary.

## Generic Harness API

The generic Harness declaration contains an ordered adapter list:

```yaml
workload:
  image: ghcr.io/private-kagent/kagent-go-adk@sha256:<digest>
  extensions:
    - name: policy-gate
      image: ghcr.io/archestra-ai/appa-adapter-kagent@sha256:<digest>
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
      adapter:
        runtimeUrlEnv: APPA_RUNTIME_URL
        runtimeBinding:
          runtimeId: customer-support-runtime-a
          policyRevision: customer-support-policy-v1
          remoteIdentity: spiffe://policy.example/runtime/customer-support-runtime-a
          caConfigMapRef: runtime-gateway-ca-v1
      ledger:
        volumeName: policy-gate-delivery-ledger
        mountPath: /var/lib/appa-adapter-kagent
      egress:
        - host: runtime-gateway.policy.example
          port: 443
      readiness:
        path: /readyz
        port: 8082
```

The generic compiler validates image and manifest digests, signatures, references, protocol ranges, socket uniqueness, ledger ownership, resource limits, egress shape, and ordering. It treats adapter and runtime configuration as opaque. The immutable Revision stores only their digests and generated references.

The quickstart manifest adds a runtime process and mounts to the companion container. Its runtime listener remains `127.0.0.1`. The remote manifest adds no runtime volume.

## Neutral protocol

The `kagent.extension.v1` protocol defines these generic RPCs:

| RPC | Purpose |
|---|---|
| `GetExtensionInfo` | Return adapter artifact, protocol, manifest, and ledger schema identity |
| `Negotiate` | Select protocol and capabilities for one host inventory |
| `Activate` | Accept the immutable Revision and host inventory digest |
| `InvokePhase` | Relay one immutable generic host or lifecycle event |
| `Quiesce` | Seal adapter delivery state and relay lifecycle quiescence |
| `ValidateRestore` | Validate restored host, adapter, and runtime bindings |
| `Shutdown` | Drain and terminate cleanly |

The protocol contains neutral types only. It contains no Google ADK Go types, OpenAPPA policy types, or runtime types.

The launcher creates one ephemeral Actor CA, host certificate, adapter certificate, boot epoch, and message-authentication key per activation. The Unix socket uses mTLS. Certificate SANs bind Actor UID, Revision, container identity, extension ID, and boot epoch.

Every event and decision carries protocol major, actor, extension, boot epoch, channel binding digest, event ID, event digest, Revision, scope, sequence, deadline, and HMAC-SHA256 over deterministic protobuf bytes. The adapter verifies peer identity, HMAC, deadline, sequence, and binding before it sends `/hook`.

## Hook relay and revision binding

The adapter converts a neutral phase event to the existing hook request codec. It adds a transport binding outside the policy payload:

```json
{
  "runtime_id": "customer-support-runtime-a",
  "policy_revision": "customer-support-policy-v1",
  "actor_uid": "actor-opaque-id",
  "extension_id": "policy-gate",
  "adapter_artifact_digest": "sha256:...",
  "kagent_revision": "revision-opaque-id",
  "boot_epoch": "epoch-opaque-id",
  "sequence": 42,
  "event_id": "event-opaque-id",
  "event_digest": "sha256:...",
  "channel_binding_digest": "sha256:...",
  "deadline": "2026-09-01T00:00:00Z"
}
```

The runtime authenticates and validates this binding before it admits the hook event. Its response contains the same binding, the accepted runtime identity, the accepted policy revision, response digest, and terminal hook decision.

The adapter maps only a fully matched runtime response to `permit`, `suppress`, `hold`, `interaction`, `commit`, `event`, or `fail`. It does not derive a fallback decision. An unavailable runtime produces the generic content-free failure or scope freeze required by the phase.

The adapter ledger returns the original response only for an identical event digest and complete binding. Runtime recovery remains authoritative for any unresolved or semantically indeterminate event.

## kagent integration points

The kagent fork uses public Google ADK interfaces and kagent-owned code only:

| Boundary | Generic integration |
|---|---|
| Public callbacks | One content-bearing out-of-process proxy |
| Provider-final catalog and request | Kagent-owned adapter before telemetry and network transmission |
| Tool call | Immutable raw argument snapshot, execution wrapper, and terminal result relay |
| Model response and event | Gate before dispatch, persistence, yield, or publication |
| Session and A2A | Exact-byte pre-append and pre-publication barriers |
| Child, memory, MCP, remote A2A | Public-interface wrappers and generic lifecycle phases |
| Snapshot, readiness, egress, drain | Generic kagent and Substrate lifecycle interface |

An unavailable required boundary rejects activation. Protected workloads disable opaque provider paths, direct content callbacks, automatic memory preload, unbounded tool catalogs, unverified MCP transport, content-bearing telemetry, unpinned remote A2A paths, and streaming before the publication gate.

The generic host executes only a matching immutable `permit` payload. Raw output reaches no protected sink before a matching `commit`. Generic extension ordering and phase ownership remain immutable for one Revision.

## Runtime mapping

`appa-runtime` receives the adapter's `/hook` event. It validates the bound runtime identity and policy revision. It maps the event to runtime admission and owns all OpenAPPA behavior:

- Policy parsing and `[deployment]` validation.
- Engine decisions, Value and Label handling, and trajectory state.
- Annotator, Membership Resolver, Authority, and Sanitizer consults.
- Canonical call and result correlation, remedy plans, and interactions.
- Child, remote delegation, assistant response, audit, and recovery behavior.
- `appa.db`, policy migrations, and runtime state generations.

The adapter has no policy-specific dynamic tool. A runtime remedy or interaction remains runtime state until its bound hook result crosses the adapter. The host receives only a neutral interaction or terminal decision.

## State, recovery, and snapshots

| Store | State |
|---|---|
| kagent host | ADK sessions, A2A tasks, generic host event IDs, and host outbox records |
| `appa-adapter-kagent` | Negotiation, channel HMAC state, event digest, replay status, delivery status, lifecycle relay status, and expiry |
| `appa-runtime` | Policy identity, `appa.db`, Engine facts, trajectory state, consult evidence, remedies, interactions, delegation, audit, and recovery |

The adapter ledger cannot become a policy cache. It never persists a semantic decision or continuation.

During recovery, kagent reconstructs host-native work and sends unresolved generic events to the adapter. The adapter re-establishes the pinned runtime binding and relays each event to `/hook`. `appa-runtime` resolves replay and indeterminate outcomes. The adapter maps the bound result back to the generic decision wire.

During snapshots, the host stops new work and calls `Quiesce`. The adapter seals its generic ledger and relays the lifecycle event. The runtime seals and validates `appa.db` and its trajectory generation. Restore readiness requires matching host, adapter, runtime, and policy-revision bindings. The adapter has no authority to accept an `appa.db` generation.

## Security and network boundary

Admission requires signed OCI image and manifest artifacts, an SBOM, and provenance. The generic trust policy pins the allowed registry, signer identity, and minimum provenance level. Tag-only images, mismatched referrers, unsigned manifests, and Revision overrides fail admission.

The adapter uses a dedicated non-root UID, read-only root filesystem, `allowPrivilegeEscalation: false`, no Linux capabilities, and RuntimeDefault seccomp. The adapter ledger uses single-writer fencing. The runtime volume uses separate fencing and belongs only to runtime.

Default-deny CNI egress permits cluster DNS and the authenticated egress gateway. Remote profile egress permits only the bound HTTPS runtime gateway. The gateway enforces workload identity, destination, TLS identity, redirect, IP-range, and DNS-rebinding policy.

Quickstart does not use network egress to reach the runtime. Its `APPA_RUNTIME_URL` is loopback only. This profile does not weaken loopback binding.

## Revision drain

Every live scope pins the adapter artifact, adapter manifest, generic protocol, runtime identity, policy revision, and runtime authentication binding. A change creates a new Revision.

New roots use the new binding after its adapter and runtime report ready. Existing scopes remain on the previous binding through terminal state or drain deadline. No between-turn hot swap exists. A rollback is another new-root routing transition. It does not move a live scope or merge runtime state.

## PR sequence

### Generic kagent fork

| PR | Change |
|---|---|
| 1 | Generic Harness API, adapter manifests, Revision data, and signature policy |
| 2 | Adapter containers, private sockets, adapter-ledger volumes, egress, and readiness |
| 3 | Neutral protocol, negotiation, health, HMAC, and generic extension host |
| 4 | Provider-final request and catalog gates in kagent-owned adapters |
| 5 | Sole content-bearing callback proxy and kagent-owned tool wrappers |
| 6 | Model response gates, session barriers, A2A publication barriers, and content-free telemetry |
| 7 | Agent, Runner, memory, remote, MCP, interaction, snapshot, recovery, and drain wrappers |

Each generic PR uses neutral fake adapters. Production packages have no OpenAPPA import or name. No PR changes Google ADK source or imports an ADK `internal` package.

### OpenAPPA repository

| PR | Change |
|---|---|
| 1 | `appa-adapter-kagent-protocol`, `appa-adapter-kagent`, UDS handshake, HMAC, and neutral inventory relay |
| 2 | `/hook` request and response binding, `APPA_RUNTIME_URL`, fail-closed transport, and runtime identity validation |
| 3 | Generic event relay, replay and delivery ledger, lifecycle relay, and quickstart companion image |
| 4 | Remote HTTPS gateway authentication, CA pinning, runtime identity, policy-revision binding, manifest, SBOM, and provenance |
| 5 | Runtime `/hook` mappings for tool, model, event, child, memory, MCP, remote, interaction, recovery, and snapshot phases |

The runtime PRs retain exclusive ownership of policy, Engine, consults, remedies, trajectory state, recovery semantics, and `appa.db`.

## Verification matrix

### Generic host tests

- Manifest signature, adapter digest, protocol range, capability, and ordering validation.
- Socket identity, launch-secret, mTLS, HMAC, event digest, deadline, sequence, and channel-binding validation.
- Immutable payload, permit, commit, event, hold, interaction, and fail decision validation.
- Required adapter health, activation, pin-and-drain, and incomplete-coverage refusal.
- No Google ADK fork, vendor tree, source copy, patch, module replacement, or `internal` import.

### Adapter tests

- No link dependency from `appa-adapter-kagent` to `appa-runtime` or `appa-engine`.
- No adapter policy parser, Engine call, policy schema migration, or `appa.db` path.
- Harness event to `/hook` wire mapping and response-to-neutral-decision mapping.
- Quickstart loopback URL, loopback-only runtime listener, and unavailable runtime fail-closed behavior.
- Remote HTTPS CA pinning, workload authentication, runtime identity, Actor, extension, artifact, and policy-revision binding.
- Rejected redirect, invalid certificate, stale epoch, duplicate event, changed event digest, changed runtime identity, and changed policy revision.
- Generic ledger replay and delivery behavior without policy or trajectory records.
- Quiesce, restore, shutdown, and drain lifecycle relay behavior.

### Runtime and end-to-end tests

- Runtime ownership of policy, Engine, consults, remedies, trajectory state, recovery, and `appa.db`.
- Tool allow, replacement, suppression, terminal failure, timeout, and crash-window behavior through the adapter.
- Model, session, child, A2A, memory, MCP, remote, interaction, and assistant-response gate coverage.
- Replay of terminal runtime decisions and runtime handling of indeterminate execution.
- Separate kagent host, adapter, and runtime state inspection.
- Quickstart two-process companion acceptance with runtime loopback binding.
- Remote gateway acceptance with authenticated HTTPS and revision-pinned runtime routing.
- Prohibited-content checks across logs, traces, adapter ledger, host state, A2A output, and snapshots.

The proof of completion requires every required capability to pass. No partial-capability mode reports ready.
