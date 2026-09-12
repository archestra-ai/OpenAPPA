---
title: Runtime checkpoints
category: Integrations
order: 8
description: Create a durable root checkpoint and fork its decision-relevant state without copying active permissions.
---

`POST /proxy/v1/checkpoints` creates a checkpoint or opens an independent root
from an existing checkpoint. It is available only when `APPA_PROXY_TOKEN` is
configured and requires the same bearer token as `POST /proxy/v1/events`.

## Experimental secure v1 API

The authenticated proxy API is experimental. `APPA_PROXY_TOKEN` must contain at
least 32 characters. Each request must send that value as a bearer token. When
the token is configured, the runtime disables `/hook` and `/mcp` so callers
cannot bypass proxy receipts or approval checks.

`POST /proxy/v1/events` and `POST /proxy/v1/checkpoints` accept at most 128
KiB. A larger request receives HTTP `413`. Responses are also bounded to 128
KiB. An optional `APPA_PROXY_APPROVAL_SECRET` signs human approval grants. It
must differ from the transport token.

The proxy derives internal `kagent:` trajectory identifiers from host root
identifiers. Do not share the proxy token with untrusted callers. Token access
controls checkpoint creation and fork requests; it is not reviewer authentication.

`GET /proxy/v1/openapi.json` publishes the complete machine-readable schema
for the authenticated runtime API. Generate the integration client from that
endpoint rather than maintaining a separate request schema.

## Create a checkpoint

Send the proxy protocol version, `kagent` adapter name, and existing host root
identifier:

```json
{
  "protocol": 1,
  "adapter": "kagent",
  "operation": "create",
  "root_id": "source-run"
}
```

The root must be quiescent. The runtime refuses roots with pending work that
cannot be included in a detached opening.

The response identifies a server-owned checkpoint:

```json
{
  "checkpoint_id": "checkpoint-...",
  "source_scope": { "adapter": "kagent", "root_id": "source-run" },
  "position": 17,
  "digest": "sha256:..."
}
```

`position` identifies the accepted log prefix. The digest binds that prefix and
its source root. Neither field is a provider-message signature. The checkpoint
identifier is opaque; the caller must not construct one from client metadata.

## Fork a root

Use the checkpoint identifier and an unused host root identifier:

```json
{
  "protocol": 1,
  "adapter": "kagent",
  "operation": "fork",
  "checkpoint_id": "checkpoint-...",
  "root_id": "fork-run"
}
```

The response is `{"root_id":"fork-run"}`. The new root has its own log and
does not alias the source. The source log remains unchanged.

Repeating the same fork request returns the same result without resetting the
target's progress. A different checkpoint cannot reuse that target identifier.
An ordinary existing root cannot become a fork target. The proxy accepts only
its `kagent` adapter namespace.

## What the checkpoint contains

The checkpoint contains the validated Label, completed policy effects and
denials needed to continue flow decisions. It does not contain raw Values,
model messages or the complete trajectory event log.

It does not copy open dispatches, standing offers, prepared approvals or
harness grants. Opening a fork does not execute a tool or approve a new action.
The runtime validates the stored policy key and snapshot before target creation.

## Integration responsibilities

This endpoint checkpoints a runtime root. It does not map client-side fork
ordinals, reconstruct provider history or authenticate client correlation IDs.

The integration must establish which checkpoint belongs to the client context
being forked. It must reject ambiguous or unverified relationships before it
opens a new root or sends that context to a model.

Compaction is a different operation. A shorter model transcript must not reset
the runtime's restrictions. This API does not authorize that reset or provide
permission to discard outstanding obligations.

See [Add to your agent](/writing-an-integration) for lifecycle events, and
[Authorities](/contracts#authorities) for authenticated review responsibilities.
