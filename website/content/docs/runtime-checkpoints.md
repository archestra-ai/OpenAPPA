---
title: Runtime checkpoints
category: Integrations
order: 8
description: Create a durable root checkpoint and fork its decision-relevant state without copying active permissions.
---

`POST /checkpoint` is a loopback-only endpoint for a trusted adapter. It creates
a checkpoint or opens an independent root from an existing checkpoint.

The runtime validates the served adapter and host root identifier. It derives
the internal trajectory identifier from that adapter. Do not expose this
endpoint to untrusted callers. Loopback access is not reviewer authentication.

## Create a checkpoint

Send the served adapter name and the existing host root identifier:

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
An ordinary existing root cannot become a fork target. A checkpoint from
another adapter namespace is refused.

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
