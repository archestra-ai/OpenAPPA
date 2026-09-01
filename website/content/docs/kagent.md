---
title: kAgent
nav_title: kAgent
category: Integrations
order: 6
description: Proposal for an OpenAPPA adapter for kagent without forking Google ADK.
---

:::proposal
name: kAgent
date: 2026-08-27
:::

This proposal adds a generic out-of-process extension host to the kagent Go fork.

OpenAPPA uses `appa-adapter-kagent` as the kagent extension. The adapter maps the generic kagent harness wire to the existing `appa-runtime` `/hook` HTTP interface. The adapter does not link `appa-runtime`, own policy, call the Engine, or own `appa.db`.

`appa-runtime` is a separate logical process. It owns policy loading, the Engine, consults, remedy plans, trajectory state, recovery semantics, and `appa.db`. The adapter fails closed when `APPA_RUNTIME_URL` is unavailable or rejects a bound request.

The kagent fork contains no OpenAPPA policy, Label, trajectory, remedy, consult, runtime, or persistence logic. Google ADK remains an unmodified upstream dependency. The design does not fork, vendor, copy, patch, replace, or import internal Google ADK packages.

This proposal uses [kagent commit `9e246fd37`](https://github.com/kagent-dev/kagent/commit/9e246fd3797457b18fc277680be1629a0f57fce0) as its source baseline. It also uses Google ADK Go 2.2.0 and Substrate 0.0.20.

OpenAPPA policy semantics remain in [How it works](/how-it-works) and [Policy contracts](/contracts).

## Ownership

| Component | Ownership |
|---|---|
| kagent fork | Neutral extension protocol, Harness Revision, public-ADK wrappers, immutable payload snapshots, host sink barriers, and host-native session and A2A state |
| `appa-adapter-kagent` | Generic kagent protocol negotiation, channel HMAC state, replay and delivery ledger, lifecycle relay, and harness-wire to `/hook` mapping |
| `appa-runtime` | Policy, Engine, consults, remedy plans, trajectory state, recovery semantics, runtime API, and `appa.db` |

The adapter has no OpenAPPA semantic state. Its replay and delivery ledger contains only generic event and delivery bindings. It contains no policy decision, Label, trajectory fact, consult result, remedy, continuation, dispatch identifier, or `appa.db` data.

The kagent fork sees only generic host payloads, phase names, digests, deadlines, decisions, and opaque handles. It does not parse OpenAPPA configuration or response meaning.

## Generic kagent changes

The fork implements generic extension infrastructure only:

- Dynamic extension configuration and artifact verification.
- Version and capability negotiation.
- Dynamic proxies for existing public ADK callbacks.
- Generic wrappers around public ADK model, tool, session, memory, and Runner interfaces.
- Generic barriers in kagent-owned provider, tool, A2A, publication, and lifecycle code.
- Generic execution, event, interaction, and lifecycle decisions.
- Generic sidecar readiness, egress, volume, snapshot, and drain handling.

The fork has no OpenAPPA crate, Go package, protobuf package, policy schema, wire enum, generated type, special tool route, or database migration. A missing public ADK boundary makes its generic capability unavailable. A required adapter then rejects activation.

The protected instance enforces four contracts:

1. The host executes only the immutable payload permitted by the required extension.
2. Raw tool and child results remain withheld until the extension commits a delivery payload.
3. Every enabled model, session, A2A, memory, UI, log, telemetry, and storage path crosses an exclusive extension gate.
4. The instance remains unready when its generic inventory cannot satisfy every required adapter capability.

## Adapter and runtime boundary

The kagent extension protocol remains neutral. It uses private Unix-domain-socket gRPC, mTLS, a boot epoch, sequence numbers, a channel digest, deadlines, and HMAC-SHA256. The protocol carries no OpenAPPA domain type.

The adapter receives one immutable generic event. It validates peer identity, Revision, boot epoch, sequence, deadline, event digest, and HMAC. It records a generic replay or delivery entry, then maps the event to one `/hook` request. It maps the runtime response back to one generic host decision.

The adapter binds every `/hook` request to these values:

- Actor UID and extension ID.
- Immutable kagent Revision and adapter artifact digest.
- Runtime policy revision and runtime instance identity.
- Boot epoch, sequence, event ID, event digest, and deadline.
- Authenticated channel identity and request HMAC digest.

`appa-runtime` verifies this binding before it evaluates a flow. A runtime response binds the same values and its policy revision. The adapter rejects a missing, mismatched, expired, reordered, or duplicate binding. A policy revision change creates a new kagent Revision and follows pin-and-drain.

The adapter does not interpret `permit`, `commit`, `hold`, `interaction`, `event`, or `fail` as policy. It only verifies the runtime response binding and renders the matching neutral host decision.

## Deployment profiles

### Quickstart

Quickstart uses one digest-pinned OpenAPPA companion container with two processes:

```text
+---------------------- OpenAPPA companion container -----------------------+
| appa-adapter-kagent                  appa runtime --adapter kagent        |
| kagent UDS gRPC <----adapter----> http://127.0.0.1:8787/hook <----runtime |
| generic ledger                         policy, Engine, consults, appa.db  |
+---------------------------------------------------------------------------+
```

`APPA_RUNTIME_URL=http://127.0.0.1:8787` selects the loopback runtime. The runtime listens only on loopback in this profile. Container networking does not expose the runtime port. The adapter has no `appa.db` mount. Only `appa-runtime` mounts the runtime policy, credentials, and durable `appa.db` volume.

### Remote configuration

Remote configuration uses the same `appa-adapter-kagent` image. `APPA_RUNTIME_URL` identifies an authenticated HTTPS runtime gateway or runtime instance.

```text
kagent host -- private UDS --> appa-adapter-kagent -- mTLS HTTPS --> appa-runtime gateway
```

The adapter authenticates the remote peer with a pinned CA and workload identity. The remote runtime authenticates the adapter identity and accepts only the configured Actor, extension, and Revision binding. The gateway forwards only to the bound runtime instance. The adapter rejects redirects, unpinned certificates, identity mismatch, and revision mismatch. Runtime policy and `appa.db` remain at the remote runtime.

Both profiles use the same `/hook` request and response binding. Remote configuration changes do not relax the fail-closed rule.

## OCI companion container

The selected process model is a digest-pinned companion container in the same Substrate Actor. The kagent process and adapter communicate through a private Unix-domain socket. A shared memory-backed socket volume carries no durable runtime state.

Substrate supports [multiple containers with independent readiness probes](https://github.com/kagent-dev/substrate/blob/v0.0.20/pkg/api/v1alpha1/actortemplate_types.go#L243-L356). Pinned kagent currently emits [one container](https://github.com/kagent-dev/kagent/blob/9e246fd3797457b18fc277680be1629a0f57fce0/go/core/v2/substrate/actor_template.go#L32-L91).

The generic fork adds extension containers without an OpenAPPA-specific role. The adapter image runs as a dedicated UID with a read-only root filesystem and no Linux capabilities. Its generic ledger has a separate volume. The kagent container cannot mount it.

Quickstart gives `appa-runtime` a separate private policy and `appa.db` volume. The adapter cannot mount either volume. Remote configuration has no runtime volume in the adapter container.

Actor-wide CNI policy permits only cluster DNS and an authenticated egress gateway. The gateway enforces per-container destination, TLS identity, redirect, and DNS-rebinding rules. The kagent process and its in-process tools remain in the trusted computing base.

## Dynamic extension configuration

A Harness can declare several ordered dynamic extensions. The OpenAPPA declaration names an adapter artifact, not a policy runtime artifact:

```yaml
extensions:
  - name: policy-gate
    image: ghcr.io/archestra-ai/appa-adapter-kagent@sha256:<digest>
    manifestDigest: sha256:<digest>
    protocol: kagent.extension.v1
    required: true
    failureMode: closed
    order: 10
    socket: /run/kagent/extensions/policy-gate.sock
    adapter:
      runtimeUrlEnv: APPA_RUNTIME_URL
      runtimeBinding:
        runtimeId: customer-support-runtime-a
        policyRevision: customer-support-policy-v1
        remoteIdentity: spiffe://policy.example/runtime/customer-support-runtime-a
    ledger:
      durableDir: policy-gate-delivery-ledger
    egress:
      - runtime-gateway.policy.example:443
    readiness:
      path: /readyz
      port: 8082
```

The generic compiler verifies OCI digest, signature policy, manifest digest, protocol range, references, capability claims, deadlines, limits, socket uniqueness, ledger ownership, egress, and readiness declarations. It treats adapter and runtime configuration as opaque bytes. An immutable Revision binds their digests. Any policy, credential, CA, runtime identity, or remote endpoint change creates a new Revision.

## Negotiation and event relay

The adapter serves the private socket before registration. The host verifies adapter workload identity and per-launch channel secret. It calls `GetExtensionInfo`, sends the complete generic capability and sink inventory, and activates the adapter after it accepts the inventory digest.

The inventory describes mechanics, not OpenAPPA policy. It includes ADK and provider versions, callback phase ordering, tool and model paths, raw codecs, content-bearing sinks, and snapshot, recovery, and drain capabilities.

The adapter sends the inventory activation event to `/hook`. `appa-runtime` validates policy coverage and returns the bound result. The adapter reports ready only after a valid runtime response. The host treats that response as a generic required-extension attestation.

Every generic event contains an immutable payload, event ID, digest, Actor UID, Revision, scope, operation, descriptor, sequence, phase, and deadline. The host executes no replacement without a matching `permit`. It publishes no content without a matching `commit` or `event` decision.

The adapter ledger makes relays replay-safe. A repeated event ID returns the original bound decision only after the adapter verifies the identical event digest and runtime revision. Runtime recovery remains authoritative. The adapter does not classify semantic outcomes or reconstruct trajectory state.

## Lifecycle coverage

The protected profile uses public ADK interfaces and kagent-owned integration points. It disables any path that bypasses them.

| Boundary | kagent-owned integration |
|---|---|
| Tool callbacks and execution | Sole content-bearing proxy, immutable arguments, execution wrapper, and result gate |
| User input and sessions | Callback proxy and exact-byte pre-append gate |
| Model request and catalog | Provider-final request and descriptor gate |
| Model response and publication | Standard and live-path gates before persistence or yield |
| Child, memory, MCP, and remote A2A | Generic wrappers with immutable request and terminal-result gates |
| Snapshot, readiness, egress, and drain | Generic kagent and Substrate lifecycle interface |

The adapter relays generic events for every required phase. `appa-runtime` maps those events to policy evaluation, Engine admission, consults, remedies, trajectory transitions, and recovery. The adapter owns no dynamic OpenAPPA tool, response meaning, interaction continuation, delegation state, or audit record.

## State and recovery

| Store | Contents |
|---|---|
| kagent | ADK sessions, A2A tasks, and narrow generic host delivery records |
| `appa-adapter-kagent` | Generic protocol negotiation, HMAC channel state, event digest, replay, delivery, expiry, and lifecycle relay records |
| `appa-runtime` | Policy identity, `appa.db`, Engine facts, trajectory state, consult pins, remedies, interactions, delegations, audit records, and recovery state |

The adapter ledger records no OpenAPPA semantic value. The runtime owns all migration of policy and `appa.db`. The adapter artifact can drain independently, but it cannot migrate runtime state.

Before a snapshot, the host stops new work and asks each required adapter to quiesce. The adapter seals its generic ledger and relays lifecycle events to runtime. `appa-runtime` seals and validates its own state generation. A restore remains unready until the host, adapter, and runtime accept the same Actor, extension, adapter artifact, runtime identity, and policy revision bindings.

A crash before adapter acknowledgement of `execution.begin` means the host did not execute the tool. A crash after that acknowledgement and before a terminal result leaves classification to `appa-runtime`. The host freezes the generic scope until the adapter relays a bound runtime `suppress`, `hold`, replayed `commit`, or terminal `fail` decision.

## Revisions and installation

One adapter artifact, protocol selection, runtime identity, and policy revision remain pinned for each live scope. Existing scopes drain on the prior binding. The host never hot-swaps an adapter or moves a live scope between runtime revisions.

Installation has three independently versioned artifacts:

1. A kagent fork with the generic dynamic extension host, public-ADK adapters, and lifecycle points.
2. The signed `appa-adapter-kagent` image and manifest.
3. The `appa-runtime` image or binary, policy revision, and runtime identity.

The generic Harness references the adapter image. It does not reference `appa.db`. Quickstart provisions the runtime process and private volume in the same companion container. Remote configuration references a gateway identity and policy revision, not a runtime database mount.

## Architecture

```text
+------------------------------ Kubernetes / Substrate Actor ------------------------------+
| kagent controller -> immutable Revision -> ActorTemplate                                 |
|                                                                                           |
| +--------------------------+ private UDS +---------------------------------------------+ |
| | kagent + upstream ADK    |<----------->| OpenAPPA companion container                 | |
| | generic extension host   |             | appa-adapter-kagent                          | |
| | public ADK wrappers      |             | generic negotiation, HMAC, delivery ledger   | |
| | sessions.db / A2A state  |             |                | APPA_RUNTIME_URL              | |
| +--------------------------+             |                v                              | |
|                                           | appa runtime --adapter kagent                | |
|                                           | policy, Engine, consults, trajectory, appa.db| |
|                                           +---------------------------------------------+ |
+-------------------------------------------------------------------------------------------+

Remote configuration replaces the final loopback hop with authenticated HTTPS to the bound runtime gateway or instance. The runtime port remains loopback-only in quickstart.

## Incomplete coverage refusal

The protected instance remains unready when a required phase lacks sole, fixed, supported ownership. The first release refuses opaque provider paths, stock MCP transport without raw arguments and no-retry execution, unbounded memory preload, mutable catalogs, unverified callbacks, content telemetry, incomplete remote A2A gates, streaming before publication control, mixed revisions, unavailable runtime identity, failed runtime authentication, and any path that needs Google ADK source changes.

The [kagent implementation plan](../../../integrations/kagent/IMPLEMENTATION.md) defines the neutral protocol, ownership boundary, deployment profiles, public-ADK integration, and verification matrix.
