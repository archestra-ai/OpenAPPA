#!/usr/bin/env python3
"""Authenticated change-board authority using OpenAPPA's existing consult wire.

The runtime posts a normal five-field authority consult to /approve. The broker
parks it and accepts only an authenticated, CSRF-protected /decide ruling.
"""

from __future__ import annotations

import argparse
import datetime
import hashlib
import hmac
import json
import os
import re
import secrets
import threading
import time
from dataclasses import dataclass, field
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any, Callable
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

WIRE_KEYS = {"version", "kind", "name", "declaration", "artifact"}
REVIEW_SCOPE_REQUIRED = {"root_id", "offer_id", "opening_policy_fingerprint"}
REVIEW_SCOPE_OPTIONAL = {"child_id"}
FINGERPRINT = re.compile(r"[0-9a-f]{64}\Z")


class BrokerError(ValueError):
    pass


@dataclass(frozen=True)
class Reviewer:
    id: str
    session_id: str
    email: str | None = None


@dataclass
class PendingReview:
    id: str
    action_id: str
    expires_at: float
    consult: dict[str, Any]
    created_at: float
    state: str = "pending"
    ruling: str | None = None
    actor: str | None = None
    reason: str | None = None
    condition: threading.Condition = field(default_factory=threading.Condition)


def canonical(value: Any) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True).encode("utf-8")


def action_id_for(consult: dict[str, Any]) -> str:
    """A visible digest of the exact runtime-supplied review artifact."""
    return hashlib.sha256(canonical({key: consult[key] for key in ("kind", "name", "declaration", "artifact")})).hexdigest()


class BetterAuthVerifier:
    """Verify a forwarded BetterAuth cookie without storing or logging it."""

    def __init__(self, auth_url: str, reviewer_id: str | None, reviewer_email: str | None) -> None:
        if not reviewer_id and not reviewer_email:
            raise ValueError("configure --reviewer-id or --reviewer-email")
        self.auth_url = auth_url.rstrip("/") + "/api/auth/get-session"
        self.reviewer_id, self.reviewer_email = reviewer_id, reviewer_email

    def __call__(self, cookie: str | None) -> Reviewer | None:
        if not cookie:
            return None
        request = Request(self.auth_url, headers={"Cookie": cookie, "Accept": "application/json"})
        try:
            with urlopen(request, timeout=5) as response:
                result = json.loads(response.read().decode("utf-8"))
        except (HTTPError, URLError, OSError, UnicodeDecodeError, json.JSONDecodeError):
            return None
        user = result.get("user") if isinstance(result, dict) else None
        session = result.get("session") if isinstance(result, dict) else None
        if not isinstance(user, dict) or not isinstance(session, dict):
            return None
        user_id, session_id, email = user.get("id"), session.get("id"), user.get("email")
        if not isinstance(user_id, str) or not isinstance(session_id, str):
            return None
        return Reviewer(user_id, session_id, email if isinstance(email, str) else None)

    def allowed(self, reviewer: Reviewer) -> bool:
        return (not self.reviewer_id or reviewer.id == self.reviewer_id) and (
            not self.reviewer_email or reviewer.email == self.reviewer_email
        )


class ChangeBoard:
    def __init__(
        self,
        verifier: Callable[[str | None], Reviewer | None],
        reviewer_allowed: Callable[[Reviewer], bool],
        window_s: float,
        audit_file: str | Path | None = None,
        audit_append: Callable[[dict[str, Any]], None] | None = None,
        require_review_context: bool = False,
    ) -> None:
        if window_s <= 0:
            raise ValueError("approval window must be positive")
        self.verifier, self.reviewer_allowed, self.window_s = verifier, reviewer_allowed, window_s
        self.csrf_secret = secrets.token_bytes(32)
        self.audit_file = Path(audit_file) if audit_file is not None else None
        self.audit_append = audit_append or self._append_audit
        self.require_review_context = require_review_context
        self._reviews: dict[str, PendingReview] = {}
        self._lock = threading.Lock()
        self._audit_lock = threading.Lock()

    def register(self, consult: dict[str, Any]) -> PendingReview:
        if set(consult) != WIRE_KEYS or consult.get("version") != 1 or consult.get("kind") != "authority":
            raise BrokerError("expected the exact OpenAPPA authority consult envelope")
        if consult.get("name") != "authenticated-reviewer":
            raise BrokerError("unexpected authority name")
        if not isinstance(consult.get("declaration"), dict) or not isinstance(consult.get("artifact"), dict):
            raise BrokerError("authority consult has invalid declaration or artifact")
        context = self._review_context(consult["artifact"])
        if self.require_review_context and context is None:
            raise BrokerError("trusted review context is required for human approval")
        now = time.time()
        review = PendingReview(secrets.token_urlsafe(12), action_id_for(consult), now + self.window_s, consult, now)
        with self._lock:
            self._reviews[review.id] = review
        return review

    @staticmethod
    def _review_context(artifact: dict[str, Any]) -> dict[str, Any] | None:
        """Read the frozen runtime's trusted review metadata inside artifact.

        The runtime owns `logical_action_digest` and `review_scope`; a provider
        retry ID is intentionally absent because a legitimate native retry can
        still name the same reviewed logical action.
        """
        digest = artifact.get("logical_action_digest")
        scope = artifact.get("review_scope")
        if not isinstance(digest, str) or not digest or not isinstance(scope, dict):
            return None
        if not REVIEW_SCOPE_REQUIRED <= set(scope) or not set(scope) <= REVIEW_SCOPE_REQUIRED | REVIEW_SCOPE_OPTIONAL:
            return None
        if any(not isinstance(scope[key], str) or not scope[key] for key in REVIEW_SCOPE_REQUIRED):
            return None
        if not FINGERPRINT.fullmatch(scope["opening_policy_fingerprint"]):
            return None
        if "child_id" in scope and (not isinstance(scope["child_id"], str) or not scope["child_id"]):
            return None
        return {"logical_action_digest": digest, "review_scope": dict(scope)}

    def pending(self, reviewer: Reviewer) -> list[dict[str, Any]]:
        with self._lock:
            reviews = list(self._reviews.values())
        return [self.public(review, reviewer) for review in reviews if self._current(review)]

    def public(self, review: PendingReview, reviewer: Reviewer) -> dict[str, Any]:
        artifact = review.consult["artifact"]
        declaration = review.consult["declaration"]
        return {
            "id": review.id,
            "action_id": review.action_id,
            "tool": artifact.get("tool"),
            "arguments": artifact.get("arguments"),
            "requirements": artifact.get("requirements"),
            "hint": declaration.get("hint"),
            "age_s": round(time.time() - review.created_at, 1),
            "expires_at": int(review.expires_at),
            "csrf_token": self.csrf(reviewer, review),
            **(context if (context := self._review_context(artifact)) else {}),
        }

    def csrf(self, reviewer: Reviewer, review: PendingReview) -> str:
        message = f"{reviewer.session_id}\n{review.id}\n{review.action_id}\n{int(review.expires_at)}"
        return hmac.new(self.csrf_secret, message.encode(), hashlib.sha256).hexdigest()

    def decide(self, reviewer: Reviewer, payload: dict[str, Any], csrf: str | None) -> PendingReview:
        allowed = {"id", "ruling", "reason"}
        if not set(payload).issubset(allowed) or set(payload) - {"reason"} != {"id", "ruling"}:
            raise BrokerError("decision must use the change-board id/ruling/reason shape")
        if not isinstance(payload["id"], str) or payload["ruling"] not in {"approve", "deny"}:
            raise BrokerError("ruling must be approve or deny")
        if "reason" in payload and (not isinstance(payload["reason"], str) or len(payload["reason"]) > 512):
            raise BrokerError("reason must be at most 512 characters")
        with self._lock:
            review = self._reviews.get(payload["id"])
        if review is None:
            raise LookupError("no such parked consult")
        if not csrf or not hmac.compare_digest(csrf, self.csrf(reviewer, review)):
            raise PermissionError("invalid CSRF token")
        with review.condition:
            if review.expires_at <= time.time():
                review.state = "expired"
                raise BrokerError("review expired")
            if review.state != "pending":
                raise BrokerError("review was already decided")
            # A ruling does not exist until its durable audit write completes.
            # The record intentionally excludes cookies, CSRF tokens, and secrets.
            record = {
                "event": "authenticated_hitl_decision",
                "decision": payload["ruling"],
                "review_id": review.id,
                "action_digest": review.action_id,
                "authenticated_reviewer_id": reviewer.id,
                "timestamp": datetime.datetime.now(datetime.timezone.utc).isoformat(),
            }
            context = self._review_context(review.consult["artifact"])
            if context is not None:
                record.update(context)
            try:
                self.audit_append(record)
            except OSError as error:
                raise BrokerError("audit write failed; decision was not granted") from error
            review.ruling, review.reason, review.actor, review.state = payload["ruling"], payload.get("reason"), reviewer.id, "decided"
            review.condition.notify_all()
        return review

    def _append_audit(self, record: dict[str, Any]) -> None:
        if self.audit_file is None:
            return
        line = canonical(record) + b"\n"
        with self._audit_lock:
            fd = os.open(self.audit_file, os.O_APPEND | os.O_CREAT | os.O_WRONLY, 0o600)
            try:
                written = 0
                while written < len(line):
                    written += os.write(fd, line[written:])
                os.fsync(fd)
            finally:
                os.close(fd)

    def await_answer(self, review: PendingReview) -> tuple[int, dict[str, Any]]:
        with review.condition:
            review.condition.wait_for(lambda: review.state != "pending", timeout=max(0, review.expires_at - time.time()))
            if review.state == "pending":
                review.state = "expired"
            if review.state != "decided":
                return HTTPStatus.GATEWAY_TIMEOUT, {"error": "authenticated review timed out"}
            review.state = "consumed"
            answer = {"ruling": review.ruling}
            if review.reason:
                answer["reason"] = review.reason
            return HTTPStatus.OK, {"version": 1, "answer": answer}

    @staticmethod
    def _current(review: PendingReview) -> bool:
        if review.state == "pending" and review.expires_at <= time.time():
            review.state = "expired"
        return review.state == "pending"


def make_handler(board: ChangeBoard) -> type[BaseHTTPRequestHandler]:
    class Handler(BaseHTTPRequestHandler):
        server_version = "AuthenticatedChangeBoard/1"

        def log_message(self, _format: str, *_args: Any) -> None:
            pass  # Never write cookies or approval data to an access log.

        def do_GET(self) -> None:  # noqa: N802
            reviewer = self._reviewer()
            if reviewer is None:
                return self.reply(HTTPStatus.UNAUTHORIZED, {"error": "authentication required"})
            if not board.reviewer_allowed(reviewer):
                return self.reply(HTTPStatus.FORBIDDEN, {"error": "reviewer is not authorized"})
            if self.path == "/v1/reviewer/me":
                return self.reply(HTTPStatus.OK, {"authenticated": True})
            if self.path == "/pending":
                return self.reply(HTTPStatus.OK, {"pending": board.pending(reviewer)})
            return self.reply(HTTPStatus.NOT_FOUND, {"error": "not found"})

        def do_POST(self) -> None:  # noqa: N802
            try:
                payload = self.body()
            except BrokerError as error:
                return self.reply(HTTPStatus.BAD_REQUEST, {"error": str(error)})
            if self.path == "/approve":
                if self.client_address[0] not in {"127.0.0.1", "::1"}:
                    return self.reply(HTTPStatus.FORBIDDEN, {"error": "authority caller must be loopback"})
                try:
                    return self.reply(*board.await_answer(board.register(payload)))
                except BrokerError as error:
                    return self.reply(HTTPStatus.BAD_REQUEST, {"error": str(error)})
            reviewer = self._reviewer()
            if reviewer is None:
                return self.reply(HTTPStatus.UNAUTHORIZED, {"error": "authentication required"})
            if not board.reviewer_allowed(reviewer):
                return self.reply(HTTPStatus.FORBIDDEN, {"error": "reviewer is not authorized"})
            if self.path == "/decide":
                try:
                    review = board.decide(reviewer, payload, self.headers.get("X-HITL-CSRF"))
                except LookupError as error:
                    return self.reply(HTTPStatus.NOT_FOUND, {"error": str(error)})
                except PermissionError as error:
                    return self.reply(HTTPStatus.FORBIDDEN, {"error": str(error)})
                except BrokerError as error:
                    return self.reply(HTTPStatus.CONFLICT, {"error": str(error)})
                return self.reply(HTTPStatus.OK, {"decided": review.id})
            return self.reply(HTTPStatus.NOT_FOUND, {"error": "not found"})

        def _reviewer(self) -> Reviewer | None:
            return board.verifier(self.headers.get("Cookie"))

        def body(self) -> dict[str, Any]:
            try:
                length = int(self.headers.get("Content-Length", "0"))
                body = json.loads(self.rfile.read(length).decode())
            except (ValueError, UnicodeDecodeError, json.JSONDecodeError) as error:
                raise BrokerError("request must contain a JSON object") from error
            if not isinstance(body, dict):
                raise BrokerError("request must contain a JSON object")
            return body

        def reply(self, status: int, value: dict[str, Any]) -> None:
            body = canonical(value)
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(body)

    return Handler


def serve(
    host: str,
    port: int,
    verifier: Callable[[str | None], Reviewer | None],
    window_s: float,
    audit_file: str | Path | None = None,
    require_review_context: bool = False,
) -> ThreadingHTTPServer:
    if host not in {"127.0.0.1", "::1"}:
        raise ValueError("broker must bind a loopback address")
    return ThreadingHTTPServer(
        (host, port),
        make_handler(ChangeBoard(
            verifier,
            getattr(verifier, "allowed", lambda _reviewer: True),
            window_s,
            audit_file,
            require_review_context=require_review_context,
        )),
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=18889)
    parser.add_argument("--approval-window", type=float, default=600)
    parser.add_argument("--audit-file", help="durable JSONL decision audit path on the mounted data volume")
    parser.add_argument("--require-review-context", action="store_true", help="refuse human decisions unless runtime supplied trusted scope metadata")
    parser.add_argument("--auth-url", default="http://127.0.0.1:9000")
    parser.add_argument("--reviewer-id")
    parser.add_argument("--reviewer-email")
    args = parser.parse_args()
    server = serve(
        args.host,
        args.port,
        BetterAuthVerifier(args.auth_url, args.reviewer_id, args.reviewer_email),
        args.approval_window,
        args.audit_file,
        args.require_review_context,
    )
    try:
        server.serve_forever()
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
