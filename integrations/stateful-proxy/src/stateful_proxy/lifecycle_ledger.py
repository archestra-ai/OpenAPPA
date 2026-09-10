"""Durable, fail-closed lifecycle state for the provider relay.

This is intentionally independent of OpenAPPA transport. It records the local
facts the proxy can establish and leaves authorization decisions to the runtime.
"""

from __future__ import annotations

import hashlib
import hmac
import json
import os
import secrets
import sqlite3
import threading
from dataclasses import dataclass
from pathlib import Path
from typing import Any


class LifecycleLedgerError(RuntimeError):
    """A lifecycle edge lacks an unambiguous durable predecessor."""


@dataclass(frozen=True)
class SpawnBinding:
    marker: str
    parent_trajectory: str
    parent_call_id: str
    principal_scope: str
    child_trajectory: str | None
    status: str
    runtime_binding: str


@dataclass(frozen=True)
class LedgerCall:
    status: str
    name: str
    args_fingerprint: str
    result_fingerprint: str | None
    delivered_result: Any


@dataclass(frozen=True)
class ResponseBinding:
    source_trajectory: str
    provider: str
    checkpoint_id: str
    inherited_prefix: list[Any]
    issued_item_digest: str
    bootstrap_digest: str | None
    source_scope: str
    variant: str = "full"


def canonical_json(value: Any) -> str:
    try:
        return json.dumps(value, separators=(",", ":"), ensure_ascii=True, allow_nan=False)
    except (TypeError, ValueError) as error:
        raise LifecycleLedgerError("lifecycle state must be JSON-safe") from error


def fingerprint(value: Any) -> str:
    return hashlib.sha256(canonical_json(value).encode("utf-8")).hexdigest()


class LifecycleLedger:
    """SQLite trajectory ledger with atomic one-time bindings and attempts."""

    def __init__(self, path: str | Path, anchor_key: bytes):
        if not anchor_key:
            raise ValueError("lifecycle anchor key must be non-empty")
        self.path = Path(path)
        self.path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        os.chmod(self.path.parent, 0o700)
        self._anchor_key = anchor_key
        self._lock = threading.RLock()
        self._connection = sqlite3.connect(self.path, check_same_thread=False, isolation_level=None, timeout=30)
        os.chmod(self.path, 0o600)
        with self._lock:
            self._connection.execute("PRAGMA journal_mode=WAL")
            self._connection.execute("PRAGMA busy_timeout=30000")
            self._connection.execute("PRAGMA foreign_keys=ON")
            self._connection.executescript(
                """
                CREATE TABLE IF NOT EXISTS trajectories (
                    trajectory_id TEXT PRIMARY KEY,
                    client TEXT NOT NULL,
                    principal_scope TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    parent_trajectory TEXT,
                    parent_call_id TEXT,
                    current_anchor TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS spawn_intents (
                    marker TEXT PRIMARY KEY,
                    parent_trajectory TEXT NOT NULL,
                    parent_call_id TEXT NOT NULL,
                    principal_scope TEXT NOT NULL,
                    child_trajectory TEXT,
                    status TEXT NOT NULL,
                    runtime_binding TEXT NOT NULL,
                    provider TEXT NOT NULL DEFAULT '',
                    original_args_fingerprint TEXT NOT NULL DEFAULT '',
                    UNIQUE(parent_trajectory, parent_call_id)
                );
                CREATE TABLE IF NOT EXISTS checkpoints (
                    checkpoint_id TEXT PRIMARY KEY,
                    trajectory_id TEXT NOT NULL,
                    anchor TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS lifecycle_events (
                    trajectory_id TEXT NOT NULL,
                    event TEXT NOT NULL,
                    fingerprint TEXT NOT NULL,
                    status TEXT NOT NULL,
                    payload TEXT,
                    PRIMARY KEY(trajectory_id, event, fingerprint)
                );
                CREATE TABLE IF NOT EXISTS tool_calls (
                    trajectory_id TEXT NOT NULL,
                    provider TEXT NOT NULL,
                    call_id TEXT NOT NULL,
                    name TEXT NOT NULL,
                    args_fingerprint TEXT NOT NULL,
                    status TEXT NOT NULL,
                    result_fingerprint TEXT,
                    delivered_result TEXT,
                    PRIMARY KEY(trajectory_id, provider, call_id)
                );
                CREATE TABLE IF NOT EXISTS observed_opaque_items (
                    trajectory_id TEXT NOT NULL,
                    provider TEXT NOT NULL,
                    item TEXT NOT NULL,
                    PRIMARY KEY (trajectory_id, provider, item)
                );
                CREATE TABLE IF NOT EXISTS response_bindings (
                    binding_key TEXT PRIMARY KEY,
                    source_trajectory TEXT NOT NULL, provider TEXT NOT NULL, checkpoint_id TEXT NOT NULL,
                    inherited_prefix TEXT NOT NULL, request_prefix TEXT NOT NULL,
                    issued_item_digest TEXT NOT NULL, bootstrap_digest TEXT,
                    source_scope TEXT NOT NULL, actual_message_bytes BLOB, actual_message_hash TEXT NOT NULL,
                    terminal_omission INTEGER NOT NULL DEFAULT 0
                );
                """
            )
            columns = {row[1] for row in self._connection.execute("PRAGMA table_info(spawn_intents)")}
            if "runtime_binding" not in columns:
                self._connection.execute("ALTER TABLE spawn_intents ADD COLUMN runtime_binding TEXT")
            if "provider" not in columns:
                self._connection.execute("ALTER TABLE spawn_intents ADD COLUMN provider TEXT NOT NULL DEFAULT ''")
            if "original_args_fingerprint" not in columns:
                self._connection.execute("ALTER TABLE spawn_intents ADD COLUMN original_args_fingerprint TEXT NOT NULL DEFAULT ''")
            self._migrate_response_bindings()

    @staticmethod
    def _response_binding_key(
        source_trajectory: str,
        provider: str,
        checkpoint_id: str,
        request_prefix: str,
        inherited_prefix: str,
        issued_item_digest: str,
        bootstrap_digest: str | None,
    ) -> str:
        """Scope a response binding to its exact durable provenance and history."""
        return fingerprint({
            "source_trajectory": source_trajectory,
            "provider": provider,
            "checkpoint_id": checkpoint_id,
            "request_prefix": request_prefix,
            "inherited_prefix": inherited_prefix,
            "issued_item_digest": issued_item_digest,
            "bootstrap_digest": bootstrap_digest,
        })

    def _migrate_response_bindings(self) -> None:
        """Atomically replace the legacy global-output unique index without losing rows."""
        with self._transaction():
            columns = {row[1] for row in self._connection.execute("PRAGMA table_info(response_bindings)")}
            if "binding_key" in columns:
                self._connection.execute(
                    "CREATE INDEX IF NOT EXISTS response_bindings_provider_bootstrap_idx ON response_bindings(provider, bootstrap_digest)"
                )
                self._connection.execute(
                    "CREATE INDEX IF NOT EXISTS response_bindings_source_provider_idx ON response_bindings(source_trajectory, provider)"
                )
                return

            request_prefix = "COALESCE(request_prefix, inherited_prefix)" if "request_prefix" in columns else "inherited_prefix"
            terminal_omission = "terminal_omission" if "terminal_omission" in columns else "0"
            rows = self._connection.execute(
                f"SELECT source_trajectory, provider, checkpoint_id, inherited_prefix, {request_prefix}, "
                f"issued_item_digest, bootstrap_digest, source_scope, actual_message_bytes, actual_message_hash, {terminal_omission} "
                "FROM response_bindings"
            ).fetchall()
            self._connection.execute(
                "CREATE TABLE response_bindings__context_v2 ("
                "binding_key TEXT PRIMARY KEY, "
                "source_trajectory TEXT NOT NULL, provider TEXT NOT NULL, checkpoint_id TEXT NOT NULL, "
                "inherited_prefix TEXT NOT NULL, request_prefix TEXT NOT NULL, "
                "issued_item_digest TEXT NOT NULL, bootstrap_digest TEXT, source_scope TEXT NOT NULL, "
                "actual_message_bytes BLOB, actual_message_hash TEXT NOT NULL, "
                "terminal_omission INTEGER NOT NULL DEFAULT 0)"
            )
            for row in rows:
                source, provider, checkpoint, inherited, request, digest, bootstrap, scope, message_bytes, message_hash, terminal = row
                key = self._response_binding_key(source, provider, checkpoint, request, inherited, digest, bootstrap)
                self._connection.execute(
                    "INSERT INTO response_bindings__context_v2 "
                    "(binding_key, source_trajectory, provider, checkpoint_id, inherited_prefix, request_prefix, "
                    "issued_item_digest, bootstrap_digest, source_scope, actual_message_bytes, actual_message_hash, terminal_omission) "
                    "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    (key, source, provider, checkpoint, inherited, request, digest, bootstrap, scope, message_bytes, message_hash, terminal),
                )
            copied = self._connection.execute("SELECT COUNT(*) FROM response_bindings__context_v2").fetchone()[0]
            if copied != len(rows):
                raise LifecycleLedgerError("response binding migration did not preserve every durable row")
            self._connection.execute("DROP TABLE response_bindings")
            self._connection.execute("ALTER TABLE response_bindings__context_v2 RENAME TO response_bindings")
            self._connection.execute(
                "CREATE INDEX response_bindings_provider_bootstrap_idx ON response_bindings(provider, bootstrap_digest)"
            )
            self._connection.execute(
                "CREATE INDEX response_bindings_source_provider_idx ON response_bindings(source_trajectory, provider)"
            )
            self._connection.execute(
                "CREATE TABLE IF NOT EXISTS lifecycle_ledger_migrations "
                "(migration TEXT PRIMARY KEY, source_rows INTEGER NOT NULL)"
            )
            self._connection.execute(
                "INSERT INTO lifecycle_ledger_migrations (migration, source_rows) "
                "VALUES ('response_bindings_context_key_v2', ?)",
                (len(rows),),
            )

    def close(self) -> None:
        self._connection.close()

    def ensure_root(self, trajectory_id: str, client: str, principal_scope: str) -> str:
        with self._transaction():
            row = self._connection.execute(
                "SELECT client, principal_scope, kind, current_anchor FROM trajectories WHERE trajectory_id = ?",
                (trajectory_id,),
            ).fetchone()
            if row:
                if row[:3] != (client, principal_scope, "root"):
                    raise LifecycleLedgerError("native trajectory ID conflicts with its durable root binding")
                return row[3]
            anchor = self._new_anchor(trajectory_id)
            self._connection.execute(
                "INSERT INTO trajectories VALUES (?, ?, ?, 'root', NULL, NULL, ?)",
                (trajectory_id, client, principal_scope, anchor),
            )
            self._connection.execute(
                "INSERT INTO checkpoints VALUES (?, ?, ?)",
                (f"root:{trajectory_id}", trajectory_id, anchor),
            )
            return anchor

    def trajectory_scope(self, trajectory_id: str) -> str:
        row = self._connection.execute(
            "SELECT principal_scope FROM trajectories WHERE trajectory_id = ?", (trajectory_id,)
        ).fetchone()
        if not row:
            raise LifecycleLedgerError("lifecycle parent trajectory is not durably known")
        return row[0]

    def trajectory(self, trajectory_id: str) -> tuple[str, str, str] | None:
        row = self._connection.execute("SELECT client, principal_scope, kind FROM trajectories WHERE trajectory_id = ?", (trajectory_id,)).fetchone()
        return tuple(row) if row else None

    def register_response(self, trajectory_id: str, provider: str, checkpoint_id: str, request_prefix: list[Any], inherited_prefix: list[Any], issued_item_digest: str, bootstrap_digest: str | None, actual_message_bytes: bytes) -> None:
        """Bind emitted provider history only after a runtime checkpoint exists."""
        if not all(isinstance(value, str) and value for value in (trajectory_id, provider, checkpoint_id, issued_item_digest)) or not isinstance(actual_message_bytes, bytes):
            raise LifecycleLedgerError("response binding requires durable source and actual provider bytes")
        with self._transaction():
            scope = self.trajectory_scope(trajectory_id)
            if not self._connection.execute("SELECT 1 FROM checkpoints WHERE checkpoint_id = ? AND trajectory_id = ?", (checkpoint_id, trajectory_id)).fetchone():
                raise LifecycleLedgerError("response binding requires a runtime-issued checkpoint for its source trajectory")
            issued = inherited_prefix[len(request_prefix):]
            terminal_omission = int(provider == "anthropic" and len(issued) == 1 and isinstance(issued[0], dict) and issued[0].get("type") == "message" and issued[0].get("role") == "assistant" and all(isinstance(part, dict) and part.get("type") in {"text", "thinking", "redacted_thinking"} for part in issued[0].get("content", [])))
            prefix, request, message_hash = canonical_json(inherited_prefix), canonical_json(request_prefix), "sha256:" + hashlib.sha256(actual_message_bytes).hexdigest()
            binding_key = self._response_binding_key(trajectory_id, provider, checkpoint_id, request, prefix, issued_item_digest, bootstrap_digest)
            row = self._connection.execute("SELECT inherited_prefix, request_prefix, issued_item_digest, bootstrap_digest, source_scope, actual_message_hash, terminal_omission FROM response_bindings WHERE binding_key = ?", (binding_key,)).fetchone()
            expected = (prefix, request, issued_item_digest, bootstrap_digest, scope, message_hash, terminal_omission)
            if row:
                if row != expected: raise LifecycleLedgerError("checkpoint response binding conflicts with previously emitted provider history")
                return
            self._connection.execute("INSERT INTO response_bindings (binding_key, source_trajectory, provider, checkpoint_id, inherited_prefix, request_prefix, issued_item_digest, bootstrap_digest, source_scope, actual_message_bytes, actual_message_hash, terminal_omission) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)", (binding_key, trajectory_id, provider, checkpoint_id, prefix, request, issued_item_digest, bootstrap_digest, scope, actual_message_bytes, message_hash, terminal_omission))

    def matching_response_binding(self, provider: str, request_history: list[Any], bootstrap_digest: str | None) -> ResponseBinding | None:
        """Match a full issued prefix or one registered Claude terminal-response cut."""
        rows = self._connection.execute("SELECT source_trajectory, checkpoint_id, inherited_prefix, request_prefix, issued_item_digest, bootstrap_digest, source_scope, terminal_omission FROM response_bindings WHERE provider = ? AND bootstrap_digest IS ?", (provider, bootstrap_digest)).fetchall()
        matches: list[ResponseBinding] = []
        def fresh(items: list[Any]) -> bool:
            return all(isinstance(item, dict) and item.get("type") == "message" and item.get("role") == "user" and all(isinstance(part, dict) and part.get("type") == "text" for part in item.get("content", [])) for item in items)
        for source, checkpoint, encoded_full, encoded_request, digest, bootstrap, scope, terminal in rows:
            full, request = json.loads(encoded_full), json.loads(encoded_request)
            if len(request_history) > len(full) and request_history[:len(full)] == full and fresh(request_history[len(full):]):
                matches.append(ResponseBinding(source, provider, checkpoint, full, digest, bootstrap, scope, "full"))
            elif terminal and len(request_history) >= len(request) and request_history[:len(request)] == request and fresh(request_history[len(request):]):
                matches.append(ResponseBinding(source, provider, checkpoint, full, digest, bootstrap, scope, "terminal_omission"))
        if len(matches) > 1: raise LifecycleLedgerError("native root history ambiguously matches multiple checkpointed provider responses")
        return matches[0] if matches else None


    def record_observed_opaque(self, trajectory_id: str, provider: str, items: list[Any]) -> None:
        with self._transaction():
            for item in items:
                if isinstance(item, dict) and item.get("type") in {"compaction", "reasoning"}:
                    self._connection.execute("INSERT OR IGNORE INTO observed_opaque_items VALUES (?, ?, ?)", (trajectory_id, provider, canonical_json(item)))

    def known_opaque_history(self, trajectory_id: str, provider: str, history: list[Any]) -> bool:
        opaque = [item for item in history if isinstance(item, dict) and item.get("type") in {"compaction", "reasoning"}]
        if not opaque: return True
        rows = self._connection.execute("SELECT item FROM observed_opaque_items WHERE trajectory_id = ? AND provider = ?", (trajectory_id, provider)).fetchall()
        known = {item for (item,) in rows}
        return all(canonical_json(item) in known for item in opaque)


    def opaque_history_allowed(self, trajectory_id: str, provider: str, history: list[Any]) -> bool:
        """Every opaque item must be owned locally or by this fork's exact source prefix."""
        opaque = [item for item in history if isinstance(item, dict) and item.get("type") in {"compaction", "reasoning"}]
        if not opaque:
            return True
        own = {item for (item,) in self._connection.execute("SELECT item FROM observed_opaque_items WHERE trajectory_id = ? AND provider = ?", (trajectory_id, provider))}
        source = self.fork_info(trajectory_id)
        if source:
            for (encoded,) in self._connection.execute("SELECT inherited_prefix FROM response_bindings WHERE source_trajectory = ? AND provider = ?", (source[0], provider)):
                prefix = json.loads(encoded)
                if len(history) >= len(prefix) and history[:len(prefix)] == prefix:
                    own |= {canonical_json(item) for item in prefix if isinstance(item, dict) and item.get("type") in {"compaction", "reasoning"}}
                    break
        return all(canonical_json(item) in own for item in opaque)

    def known_inherited_opaque_history(self, trajectory_id: str, provider: str, history: list[Any]) -> bool:
        """A fork may use only opaque items in one exact bound source prefix."""
        source = self.fork_info(trajectory_id)
        opaque = [item for item in history if isinstance(item, dict) and item.get("type") in {"compaction", "reasoning"}]
        if not source or not opaque:
            return False
        rows = self._connection.execute("SELECT inherited_prefix FROM response_bindings WHERE source_trajectory = ? AND provider = ?", (source[0], provider)).fetchall()
        for (encoded,) in rows:
            prefix = json.loads(encoded)
            if len(history) < len(prefix) or history[:len(prefix)] != prefix:
                continue
            bound = {canonical_json(item) for item in prefix if isinstance(item, dict) and item.get("type") in {"compaction", "reasoning"}}
            if all(canonical_json(item) in bound for item in opaque):
                return True
        return False

    def fork_info(self, trajectory_id: str) -> tuple[str, str] | None:
        row = self._connection.execute("SELECT parent_trajectory, parent_call_id FROM trajectories WHERE trajectory_id = ? AND kind = 'fork'", (trajectory_id,)).fetchone()
        return tuple(row) if row and all(isinstance(value, str) and value for value in row) else None

    def current_anchor(self, trajectory_id: str) -> str:
        row = self._connection.execute(
            "SELECT current_anchor FROM trajectories WHERE trajectory_id = ?", (trajectory_id,)
        ).fetchone()
        if not row:
            raise LifecycleLedgerError("lifecycle trajectory is not durably known")
        return row[0]

    def record_checkpoint(self, trajectory_id: str, checkpoint_id: str) -> str:
        if not checkpoint_id:
            raise LifecycleLedgerError("checkpoint ID is required")
        with self._transaction():
            anchor = self.current_anchor(trajectory_id)
            row = self._connection.execute(
                "SELECT trajectory_id, anchor FROM checkpoints WHERE checkpoint_id = ?", (checkpoint_id,)
            ).fetchone()
            if row:
                if row != (trajectory_id, anchor):
                    raise LifecycleLedgerError("checkpoint ID conflicts with durable policy state")
                return anchor
            self._connection.execute(
                "INSERT INTO checkpoints VALUES (?, ?, ?)", (checkpoint_id, trajectory_id, anchor)
            )
            return anchor

    def prepare_spawn_marker(
        self,
        parent_trajectory: str,
        parent_call_id: str,
        principal_scope: str,
        provider: str,
        original_args: Any,
    ) -> str:
        """Allocate a signed but unusable carrier before Gate admission.

        Replays must present the exact provider argument object recorded on the
        first observation. This is an alias check, not a general user-data
        rewrite facility.
        """
        if not provider:
            raise LifecycleLedgerError("spawn carrier requires a provider identity")
        original_args_fingerprint = fingerprint(original_args)
        with self._transaction():
            if self.trajectory_scope(parent_trajectory) != principal_scope:
                raise LifecycleLedgerError("spawn principal scope does not match the parent trajectory")
            row = self._connection.execute(
                "SELECT marker, principal_scope, provider, original_args_fingerprint FROM spawn_intents WHERE parent_trajectory = ? AND parent_call_id = ?",
                (parent_trajectory, parent_call_id),
            ).fetchone()
            if row:
                if row[1:] != (principal_scope, provider, original_args_fingerprint):
                    raise LifecycleLedgerError("provider replay does not match the exact recorded spawn argument alias")
                return row[0]
            nonce = secrets.token_urlsafe(24)
            signature = hmac.new(
                self._anchor_key,
                f"spawn.{parent_trajectory}.{parent_call_id}.{principal_scope}.{nonce}".encode(),
                hashlib.sha256,
            ).hexdigest()[:32]
            marker = f"spm_{nonce}_{signature}"
            self._connection.execute(
                "INSERT INTO spawn_intents(marker, parent_trajectory, parent_call_id, principal_scope, child_trajectory, status, runtime_binding, provider, original_args_fingerprint) VALUES (?, ?, ?, ?, NULL, 'inactive', '', ?, ?)",
                (marker, parent_trajectory, parent_call_id, principal_scope, provider, original_args_fingerprint),
            )
            return marker

    def activate_spawn_marker(
        self,
        parent_trajectory: str,
        parent_call_id: str,
        principal_scope: str,
        provider: str,
        original_args: Any,
        runtime_binding: str,
    ) -> str:
        """Make a preallocated carrier eligible only after real Gate allowance."""
        if not runtime_binding:
            raise LifecycleLedgerError("admitted spawn has no Gate-issued spawn binding")
        original_args_fingerprint = fingerprint(original_args)
        with self._transaction():
            if self.trajectory_scope(parent_trajectory) != principal_scope:
                raise LifecycleLedgerError("spawn principal scope does not match the parent trajectory")
            row = self._connection.execute(
                "SELECT marker, principal_scope, runtime_binding, provider, original_args_fingerprint, status FROM spawn_intents WHERE parent_trajectory = ? AND parent_call_id = ?",
                (parent_trajectory, parent_call_id),
            ).fetchone()
            if not row:
                raise LifecycleLedgerError("spawn carrier was not preallocated before Gate admission")
            if row[1] != principal_scope or row[3] != provider or row[4] != original_args_fingerprint:
                raise LifecycleLedgerError("provider replay does not match the exact recorded spawn argument alias")
            if row[5] in {"eligible", "bound"}:
                if row[2] != runtime_binding:
                    raise LifecycleLedgerError("spawn call was previously admitted for another runtime binding")
                return row[0]
            if row[5] != "inactive":
                raise LifecycleLedgerError("spawn carrier is in an unsupported lifecycle state")
            updated = self._connection.execute(
                "UPDATE spawn_intents SET status = 'eligible', runtime_binding = ? WHERE marker = ? AND status = 'inactive'",
                (runtime_binding, row[0]),
            ).rowcount
            if updated != 1:
                raise LifecycleLedgerError("spawn carrier eligibility did not match the pending admission")
            return row[0]

    def issue_spawn_marker(self, parent_trajectory: str, parent_call_id: str, principal_scope: str, runtime_binding: str) -> str:
        """Compatibility helper for direct ledger tests and legacy fixtures."""
        marker = self.prepare_spawn_marker(parent_trajectory, parent_call_id, principal_scope, "legacy", {})
        return self.activate_spawn_marker(parent_trajectory, parent_call_id, principal_scope, "legacy", {}, runtime_binding)

    def bind_child(self, marker: str, child_trajectory: str, client: str, principal_scope: str) -> SpawnBinding:
        with self._transaction():
            row = self._connection.execute(
                "SELECT parent_trajectory, parent_call_id, principal_scope, child_trajectory, status, runtime_binding FROM spawn_intents WHERE marker = ?",
                (marker,),
            ).fetchone()
            if not row:
                raise LifecycleLedgerError("child did not present an eligible proxy-issued spawn marker")
            parent, parent_call, expected_scope, bound_child, status, runtime_binding = row
            expected_signature = hmac.new(
                self._anchor_key,
                f"spawn.{parent}.{parent_call}.{expected_scope}.{marker[len('spm_'):].rsplit('_', 1)[0]}".encode(),
                hashlib.sha256,
            ).hexdigest()[:32]
            if not hmac.compare_digest(marker.rsplit("_", 1)[-1], expected_signature):
                raise LifecycleLedgerError("spawn marker signature is invalid")
            if principal_scope != expected_scope:
                raise LifecycleLedgerError("spawn marker was copied across a principal scope")
            if status in {"bound", "started"}:
                if bound_child != child_trajectory:
                    raise LifecycleLedgerError("spawn marker was already bound to a different child trajectory")
                return SpawnBinding(marker, parent, parent_call, expected_scope, bound_child, status, runtime_binding)
            if status != "eligible":
                raise LifecycleLedgerError("child presented a carrier before real Gate allowance")
            if child_trajectory == parent:
                raise LifecycleLedgerError("child trajectory cannot equal its parent trajectory")
            existing = self._connection.execute(
                "SELECT principal_scope, parent_trajectory, parent_call_id FROM trajectories WHERE trajectory_id = ?",
                (child_trajectory,),
            ).fetchone()
            expected = (principal_scope, parent, parent_call)
            if existing and existing != expected:
                raise LifecycleLedgerError("native child trajectory conflicts with a durable binding")
            if not existing:
                self._connection.execute(
                    "INSERT INTO trajectories VALUES (?, ?, ?, 'child', ?, ?, ?)",
                    (child_trajectory, client, principal_scope, parent, parent_call, self.current_anchor(parent)),
                )
            self._connection.execute(
                "UPDATE spawn_intents SET child_trajectory = ?, status = 'bound' WHERE marker = ?",
                (child_trajectory, marker),
            )
            return SpawnBinding(marker, parent, parent_call, expected_scope, child_trajectory, "bound", runtime_binding)

    def spawn_for_call(self, parent_trajectory: str, parent_call_id: str) -> SpawnBinding | None:
        row = self._connection.execute(
            "SELECT marker, principal_scope, child_trajectory, status, runtime_binding FROM spawn_intents WHERE parent_trajectory = ? AND parent_call_id = ?",
            (parent_trajectory, parent_call_id),
        ).fetchone()
        return SpawnBinding(row[0], parent_trajectory, parent_call_id, row[1], row[2], row[3], row[4]) if row else None

    def binding_for_child(self, child_trajectory: str) -> SpawnBinding | None:
        row = self._connection.execute(
            "SELECT marker, parent_trajectory, parent_call_id, principal_scope, status, runtime_binding FROM spawn_intents WHERE child_trajectory = ?",
            (child_trajectory,),
        ).fetchone()
        return SpawnBinding(row[0], row[1], row[2], row[3], child_trajectory, row[4], row[5]) if row else None

    def mark_child_started(self, child_trajectory: str) -> SpawnBinding:
        """Advance a bound child only after its runtime child_start acknowledgement."""
        with self._transaction():
            row = self._connection.execute(
                "SELECT marker, parent_trajectory, parent_call_id, principal_scope, status, runtime_binding FROM spawn_intents WHERE child_trajectory = ?",
                (child_trajectory,),
            ).fetchone()
            if not row:
                raise LifecycleLedgerError("runtime child_start has no durable child binding")
            marker, parent, parent_call, scope, status, runtime_binding = row
            if status == "started":
                return SpawnBinding(marker, parent, parent_call, scope, child_trajectory, status, runtime_binding)
            if status != "bound":
                raise LifecycleLedgerError("runtime child_start has an invalid carrier state")
            updated = self._connection.execute(
                "UPDATE spawn_intents SET status = 'started' WHERE marker = ? AND status = 'bound'",
                (marker,),
            ).rowcount
            if updated != 1:
                raise LifecycleLedgerError("runtime child_start did not match its pending binding")
            return SpawnBinding(marker, parent, parent_call, scope, child_trajectory, "started", runtime_binding)

    def mark_child_returned(self, child_trajectory: str) -> SpawnBinding:
        with self._transaction():
            row = self._connection.execute(
                "SELECT marker, parent_trajectory, parent_call_id, principal_scope, status, runtime_binding FROM spawn_intents WHERE child_trajectory = ?",
                (child_trajectory,),
            ).fetchone()
            if not row:
                raise LifecycleLedgerError("child return has no durable child binding")
            marker, parent, parent_call, scope, status, runtime_binding = row
            if status == "returned":
                return SpawnBinding(marker, parent, parent_call, scope, child_trajectory, status, runtime_binding)
            if status != "started":
                raise LifecycleLedgerError("child return is not started")
            self._connection.execute("UPDATE spawn_intents SET status = 'returned' WHERE marker = ? AND status = 'started'", (marker,))
            return SpawnBinding(marker, parent, parent_call, scope, child_trajectory, "returned", runtime_binding)

    def root_for(self, trajectory_id: str) -> str:
        """Return the root that owns a root/child/fork trajectory without guessing."""
        current = trajectory_id
        while True:
            row = self._connection.execute(
                "SELECT kind, parent_trajectory FROM trajectories WHERE trajectory_id = ?", (current,)
            ).fetchone()
            if not row:
                raise LifecycleLedgerError("lifecycle trajectory is not durably known")
            # A fork is a detached runtime root whose durable inheritance is
            # provenance only. Its descendants must never resolve to the
            # source trajectory's runtime state.
            if row[0] in {"root", "fork"}:
                return current
            if not row[1]:
                raise LifecycleLedgerError("durable lifecycle lineage is incomplete")
            current = row[1]

    def fork(self, trajectory_id: str, source_trajectory: str, checkpoint_id: str, client: str, principal_scope: str) -> str:
        with self._transaction():
            if self.trajectory_scope(source_trajectory) != principal_scope:
                raise LifecycleLedgerError("fork principal scope does not match the source trajectory")
            checkpoint = self._connection.execute(
                "SELECT anchor FROM checkpoints WHERE checkpoint_id = ? AND trajectory_id = ?",
                (checkpoint_id, source_trajectory),
            ).fetchone()
            if not checkpoint:
                raise LifecycleLedgerError("fork does not name an exact known durable checkpoint")
            row = self._connection.execute(
                "SELECT client, principal_scope, kind, parent_trajectory, parent_call_id, current_anchor FROM trajectories WHERE trajectory_id = ?",
                (trajectory_id,),
            ).fetchone()
            if row:
                if row[:5] != (client, principal_scope, "fork", source_trajectory, checkpoint_id):
                    raise LifecycleLedgerError("fork trajectory conflicts with durable lineage")
                return row[5]
            # The policy checkpoint is inherited, but the child receives its own
            # signed anchor so an anchor cannot be replayed across trajectories.
            anchor = self._new_anchor(trajectory_id)
            self._connection.execute(
                "INSERT INTO trajectories VALUES (?, ?, ?, 'fork', ?, ?, ?)",
                (trajectory_id, client, principal_scope, source_trajectory, checkpoint_id, anchor),
            )
            return anchor

    def compact(self, trajectory_id: str, previous_anchor: str) -> str:
        with self._transaction():
            current = self.current_anchor(trajectory_id)
            if not self._valid_anchor(trajectory_id, previous_anchor) or previous_anchor != current:
                raise LifecycleLedgerError("compaction anchor is invalid, stale, or belongs to another trajectory")
            next_anchor = self._new_anchor(trajectory_id)
            self._connection.execute(
                "UPDATE trajectories SET current_anchor = ? WHERE trajectory_id = ?", (next_anchor, trajectory_id)
            )
            return next_anchor

    def reserve_event(self, trajectory_id: str, event: str, value: Any) -> str:
        event_fingerprint = fingerprint(value)
        with self._transaction():
            row = self._connection.execute(
                "SELECT status FROM lifecycle_events WHERE trajectory_id = ? AND event = ? AND fingerprint = ?",
                (trajectory_id, event, event_fingerprint),
            ).fetchone()
            if row:
                if row[0] == "done":
                    return "replay"
                raise LifecycleLedgerError("a prior lifecycle event attempt is unresolved after interruption")
            self._connection.execute(
                "INSERT INTO lifecycle_events VALUES (?, ?, ?, 'intent', NULL)",
                (trajectory_id, event, event_fingerprint),
            )
            return "new"

    def complete_event(self, trajectory_id: str, event: str, value: Any, payload: Any) -> None:
        event_fingerprint = fingerprint(value)
        with self._transaction():
            updated = self._connection.execute(
                "UPDATE lifecycle_events SET status = 'done', payload = ? WHERE trajectory_id = ? AND event = ? AND fingerprint = ? AND status = 'intent'",
                (canonical_json(payload), trajectory_id, event, event_fingerprint),
            ).rowcount
            if updated != 1:
                raise LifecycleLedgerError("lifecycle event completion does not match a pending attempt")

    def has_open_call(self, trajectory_id: str) -> bool:
        row = self._connection.execute(
            "SELECT 1 FROM tool_calls WHERE trajectory_id = ? AND status = 'admitted' AND result_fingerprint IS NULL LIMIT 1",
            (trajectory_id,),
        ).fetchone()
        return row is not None

    def reserve_call(self, trajectory_id: str, provider: str, call_id: str, name: str, args: Any) -> str:
        args_fingerprint = fingerprint(args)
        with self._transaction():
            row = self._connection.execute(
                "SELECT name, args_fingerprint, status FROM tool_calls WHERE trajectory_id = ? AND provider = ? AND call_id = ?",
                (trajectory_id, provider, call_id),
            ).fetchone()
            if row:
                if row[:2] != (name, args_fingerprint):
                    raise LifecycleLedgerError("provider reused a tool call ID with different trajectory intent")
                if row[2] == "admitted":
                    return "replay"
                raise LifecycleLedgerError("a prior tool-call admission attempt is unresolved after interruption")
            self._connection.execute(
                "INSERT INTO tool_calls VALUES (?, ?, ?, ?, ?, 'intent', NULL, NULL)",
                (trajectory_id, provider, call_id, name, args_fingerprint),
            )
            return "new"

    def complete_call(self, trajectory_id: str, provider: str, call_id: str) -> None:
        with self._transaction():
            updated = self._connection.execute(
                "UPDATE tool_calls SET status = 'admitted' WHERE trajectory_id = ? AND provider = ? AND call_id = ? AND status = 'intent'",
                (trajectory_id, provider, call_id),
            ).rowcount
            if updated != 1:
                raise LifecycleLedgerError("tool-call completion does not match a pending admission")

    def call(self, trajectory_id: str, provider: str, call_id: str) -> LedgerCall:
        row = self._connection.execute(
            "SELECT status, name, args_fingerprint, result_fingerprint, delivered_result FROM tool_calls WHERE trajectory_id = ? AND provider = ? AND call_id = ?",
            (trajectory_id, provider, call_id),
        ).fetchone()
        if not row or row[0] != "admitted":
            raise LifecycleLedgerError("tool result cannot be attributed to an admitted durable tool call")
        return LedgerCall(row[0], row[1], row[2], row[3], json.loads(row[4]) if row[4] is not None else None)

    def reserve_result(self, trajectory_id: str, provider: str, call_id: str, body: Any) -> str:
        body_fingerprint = fingerprint(body)
        with self._transaction():
            call = self.call(trajectory_id, provider, call_id)
            if call.result_fingerprint:
                if call.result_fingerprint == "intent:" + body_fingerprint:
                    raise LifecycleLedgerError("a prior tool-result attempt is unresolved after interruption")
                if call.result_fingerprint != body_fingerprint:
                    raise LifecycleLedgerError("replayed tool result changed body for the same native call ID")
                return "replay"
            updated = self._connection.execute(
                "UPDATE tool_calls SET result_fingerprint = ? WHERE trajectory_id = ? AND provider = ? AND call_id = ? AND result_fingerprint IS NULL",
                ("intent:" + body_fingerprint, trajectory_id, provider, call_id),
            ).rowcount
            if updated != 1:
                raise LifecycleLedgerError("a prior tool-result attempt is unresolved after interruption")
            return "new"

    def complete_result(self, trajectory_id: str, provider: str, call_id: str, body: Any, delivered: Any) -> None:
        body_fingerprint = fingerprint(body)
        with self._transaction():
            updated = self._connection.execute(
                "UPDATE tool_calls SET result_fingerprint = ?, delivered_result = ? WHERE trajectory_id = ? AND provider = ? AND call_id = ? AND result_fingerprint = ?",
                (body_fingerprint, canonical_json(delivered), trajectory_id, provider, call_id, "intent:" + body_fingerprint),
            ).rowcount
            if updated != 1:
                raise LifecycleLedgerError("tool-result completion does not match a pending attempt")

    def _new_anchor(self, trajectory_id: str) -> str:
        nonce = secrets.token_urlsafe(24)
        signature = hmac.new(self._anchor_key, f"{trajectory_id}.{nonce}".encode(), hashlib.sha256).hexdigest()
        return f"v1.{nonce}.{signature}"

    def _valid_anchor(self, trajectory_id: str, anchor: str) -> bool:
        parts = anchor.split(".")
        if len(parts) != 3 or parts[0] != "v1":
            return False
        expected = hmac.new(self._anchor_key, f"{trajectory_id}.{parts[1]}".encode(), hashlib.sha256).hexdigest()
        return hmac.compare_digest(expected, parts[2])

    def _transaction(self):
        return _Transaction(self._lock, self._connection)


class _Transaction:
    def __init__(self, lock: threading.RLock, connection: sqlite3.Connection):
        self._lock = lock
        self._connection = connection

    def __enter__(self):
        self._lock.acquire()
        self._connection.execute("BEGIN IMMEDIATE")
        return self

    def __exit__(self, exc_type, _exc, _traceback):
        self._connection.execute("ROLLBACK" if exc_type else "COMMIT")
        self._lock.release()
