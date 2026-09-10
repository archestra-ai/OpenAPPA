"""Trusted root-only client for the detached OpenAPPA checkpoint endpoint."""

from __future__ import annotations

import http.client
import json
from dataclasses import dataclass
from urllib.parse import urlsplit


class CheckpointError(RuntimeError):
    pass


@dataclass(frozen=True)
class Checkpoint:
    checkpoint_id: str
    adapter: str
    root_id: str
    position: int
    digest: str


class CheckpointClient:
    def __init__(self, runtime_url: str, adapter: str = "kagent"):
        parsed = urlsplit(runtime_url)
        if parsed.scheme != "http" or not parsed.hostname:
            raise ValueError("checkpoint runtime URL must be an http URL with a host")
        self.host = parsed.hostname
        self.port = parsed.port or 80
        self.base_path = parsed.path.rstrip("/")
        self.adapter = adapter

    def create(self, root_id: str) -> Checkpoint:
        value = self._post({"protocol": 1, "adapter": self.adapter, "operation": "create", "root_id": root_id})
        scope = value.get("source_scope")
        if not isinstance(scope, dict) or scope.get("adapter") != self.adapter or scope.get("root_id") != root_id:
            raise CheckpointError("checkpoint response has an unexpected source scope")
        checkpoint_id = value.get("checkpoint_id")
        position = value.get("position")
        digest = value.get("digest")
        if not isinstance(checkpoint_id, str) or not isinstance(position, int) or not isinstance(digest, str) or not digest.startswith("sha256:"):
            raise CheckpointError("checkpoint response is malformed")
        return Checkpoint(checkpoint_id, self.adapter, root_id, position, digest)

    def fork(self, checkpoint_id: str, root_id: str) -> None:
        value = self._post({"protocol": 1, "adapter": self.adapter, "operation": "fork", "checkpoint_id": checkpoint_id, "root_id": root_id})
        if value != {"root_id": root_id}:
            raise CheckpointError("fork response does not bind the requested target root")

    def _post(self, payload: dict) -> dict:
        body = json.dumps(payload, separators=(",", ":")).encode()
        connection = http.client.HTTPConnection(self.host, self.port, timeout=5)
        try:
            connection.request("POST", f"{self.base_path}/checkpoint", body, {"Content-Type": "application/json", "Content-Length": str(len(body))})
            response = connection.getresponse()
            raw = response.read()
        except OSError as error:
            raise CheckpointError("checkpoint runtime is unavailable") from error
        finally:
            connection.close()
        if response.status != 200:
            raise CheckpointError("checkpoint runtime refused the request")
        try:
            value = json.loads(raw)
        except json.JSONDecodeError as error:
            raise CheckpointError("checkpoint runtime returned invalid JSON") from error
        if not isinstance(value, dict):
            raise CheckpointError("checkpoint runtime returned an invalid envelope")
        return value
