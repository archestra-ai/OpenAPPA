"""Mock external services for the kagent demo: one Annotator, two authorities, one sanitizer.

A small HTTP service answering appa-runtime's consult wire
(appa-runtime/src/external.rs, appa-runtime/src/consult.rs). The
runtime POSTs one JSON envelope per consult:

    {"version": 1, "kind": "annotation"|"authority"|..., "name": "...",
     "declaration": {...}, "artifact": {...}}

and reads back `{"version": 1, "answer": <object>}` — exactly those
two keys, an exact Content-Length, and a body under the deployment's
`max_body_bytes`. Any non-2xx status is a clean no-answer (never a
denial), so a refusal here is an HTTP 404 with a diagnostic body the
runtime never parses.

Four components, all deterministic and logged to stdout:

- POST /annotate — Annotator "runbook-readers" for `lookup_runbook`.
  Reads the runbook id from the artifact and answers a per-call
  contract: `public-*` ids get the neutral contract (`delta` empty),
  `ops-*` ids narrow the produced value to the `ops` audience, and
  any other id gets no answer.
- POST /authorize — authority "release-window", human-less. Approves
  the consulted call iff any top-level string argument equals
  "catalog-cache"; every other call is denied with a reason.
- POST /approve — authority "change-board", people out of band. Parks
  the consult until a ruling arrives on the side channel, or answers
  no-answer (504) when the approval window closes first:
    GET  /pending          — the parked consults (id, tool, arguments, hint)
    POST /decide           — {"id": ..., "ruling": "approve"|"deny", "reason"?}
  The window (--approval-window, default 25s) must sit inside the
  policy's externals.timeout_ms, so an unanswered consult is a clean
  no-answer and never a transport error.
- POST /sanitize — the derivation both demo sanitizers bind to. Answers
  the consulted body with the demo's secret values redacted and the lines
  that address the reader removed. The chart policy and integration suite
  use the same deterministic implementation without a second model.

Run: python3 mock_externals.py [--host H] [--port P] [--verbose]
"""

from __future__ import annotations

import argparse
import json
import re
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

WIRE_VERSION = 1

# How long a change-board consult waits for a ruling before it answers
# nothing. Inside the policy's externals.timeout_ms (30 s in the demo
# policy) by design.
APPROVAL_WINDOW_S = 25.0

# The one deployment whose changes sit inside the release window.
RELEASE_WINDOW_DEPLOYMENT = "catalog-cache"

# The audience an ops-* runbook narrows its reader set to. The produced
# contract may only use values from the declared mandate, so the answer
# is refused (no answer) when the consult's declaration does not carry it.
OPS_AUDIENCE = "ops"


def neutral_contract() -> dict:
    """The identity annotation: no narrowing, no requirement, no effect."""
    return {"delta": {}, "requires": {"history": [], "attention": []}, "emits": []}


def ops_contract() -> dict:
    """The produced value is readable by the ops audience only."""
    contract = neutral_contract()
    contract["delta"] = {"audience": [OPS_AUDIENCE]}
    return contract


def runbook_id(args: object) -> str | None:
    """The runbook id inside an annotation artifact's `args`.

    With no declared `inputs` the artifact carries the complete call
    (`{"name": ..., "arguments": {...}}`); with a mapped input named
    `runbook` it carries `{"runbook": ...}` directly. Both are served.
    """
    if not isinstance(args, dict):
        return None
    arguments = args.get("arguments")
    if isinstance(arguments, dict) and isinstance(arguments.get("runbook"), str):
        return arguments["runbook"]
    if isinstance(args.get("runbook"), str):
        return args["runbook"]
    return None


def annotate(declaration: object, artifact: object) -> tuple[dict | None, str]:
    """The runbook-readers decision: (answer, log detail). None is no answer."""
    args = artifact.get("args") if isinstance(artifact, dict) else None
    runbook = runbook_id(args)
    if runbook is None:
        return None, "no runbook id in the artifact"
    if runbook.startswith("public-"):
        return neutral_contract(), f"runbook={runbook} -> unrestricted"
    if runbook.startswith("ops-"):
        audiences = declaration.get("audiences") if isinstance(declaration, dict) else None
        if not isinstance(audiences, list) or OPS_AUDIENCE not in audiences:
            return None, f"runbook={runbook} -> mandate does not admit the {OPS_AUDIENCE} audience"
        return ops_contract(), f"runbook={runbook} -> audience [{OPS_AUDIENCE}]"
    return None, f"runbook={runbook} -> no contract"


def authorize(artifact: object) -> tuple[dict, str]:
    """The release-window ruling: approve only the catalog-cache change."""
    tool = artifact.get("tool") if isinstance(artifact, dict) else None
    arguments = artifact.get("arguments") if isinstance(artifact, dict) else None
    named = [value for value in arguments.values() if isinstance(value, str)] if isinstance(arguments, dict) else []
    if RELEASE_WINDOW_DEPLOYMENT in named:
        return (
            {"ruling": "approve", "reason": f"{RELEASE_WINDOW_DEPLOYMENT} is inside the release window"},
            f"tool={tool} deployment={RELEASE_WINDOW_DEPLOYMENT} -> approve",
        )
    return (
        {"ruling": "deny", "reason": "only catalog-cache restarts are inside the release window"},
        f"tool={tool} arguments={json.dumps(arguments, sort_keys=True)} -> deny",
    )


# The demo's secret material, by prefix: the payments API key and the
# webhook secret `read_secret` returns.
SECRET_VALUE = re.compile(r"(?:pk_live|whsec)_[A-Za-z0-9]+")
REDACTION = "[redacted]"

# What marks a line as addressed to the reader rather than stating a
# fact. The demo's crash logs and status page carry one each.
INSTRUCTION_MARKERS = ("ignore your previous instructions", "SYSTEM:")


def drop_instructions(body: str) -> str:
    """The body without the lines that address the reader.

    A line carrying a marker goes, and so do the indented lines that
    continue it — the demo's crash log wraps its injection over two
    lines, and half an instruction is still an instruction.
    """
    kept: list[str] = []
    dropping = False
    for line in body.splitlines():
        if any(marker in line for marker in INSTRUCTION_MARKERS):
            dropping = True
            continue
        if dropping and line[:1].isspace():
            continue
        dropping = False
        kept.append(line)
    return "\n".join(kept)


def sanitize(artifact: object) -> tuple[dict | None, str]:
    """The sanitizer derivation: (answer, log detail). None is no answer.

    The artifact carries the value under `body`, and the tool that
    produced it under `tool` where one did — a child return names none,
    so the derivation reads the body alone.
    """
    body = artifact.get("body") if isinstance(artifact, dict) else None
    if not isinstance(body, str):
        return None, "no body in the artifact"
    derived = SECRET_VALUE.sub(REDACTION, drop_instructions(body))
    tool = artifact.get("tool")
    return {"body": derived}, f"tool={tool} body={len(body)}b -> {len(derived)}b"


class ChangeBoard:
    """The parked consults of the change-board authority, and their rulings."""

    def __init__(self, window_s: float):
        self.window_s = window_s
        self._cond = threading.Condition()
        self._parked: dict[str, dict] = {}

    def consult(self, declaration: object, artifact: object) -> tuple[dict | None, str]:
        """Park one consult until a ruling arrives or the window closes."""
        entry = {
            "id": uuid.uuid4().hex[:12],
            "tool": artifact.get("tool") if isinstance(artifact, dict) else None,
            "arguments": artifact.get("arguments") if isinstance(artifact, dict) else None,
            "hint": declaration.get("hint") if isinstance(declaration, dict) else None,
            "created": time.time(),
            "ruling": None,
            "reason": None,
        }
        with self._cond:
            self._parked[entry["id"]] = entry
            deadline = entry["created"] + self.window_s
            while entry["ruling"] is None and time.time() < deadline:
                self._cond.wait(timeout=max(0.0, deadline - time.time()))
            self._parked.pop(entry["id"], None)
        if entry["ruling"] is None:
            return None, f"request={entry['id']} tool={entry['tool']} -> unanswered within {self.window_s:.0f}s"
        reason = entry["reason"] or f"the change board ruled {entry['ruling']}"
        return {"ruling": entry["ruling"], "reason": reason}, f"request={entry['id']} tool={entry['tool']} -> {entry['ruling']}"

    def pending(self) -> list[dict]:
        with self._cond:
            now = time.time()
            return [
                {"id": e["id"], "tool": e["tool"], "arguments": e["arguments"], "hint": e["hint"], "age_s": round(now - e["created"], 1)}
                for e in self._parked.values()
            ]

    def decide(self, request_id: object, ruling: object, reason: object) -> bool:
        if ruling not in ("approve", "deny"):
            return False
        with self._cond:
            entry = self._parked.get(request_id) if isinstance(request_id, str) else None
            if entry is None or entry["ruling"] is not None:
                return False
            entry["ruling"] = ruling
            entry["reason"] = reason if isinstance(reason, str) else None
            self._cond.notify_all()
        return True


class ConsultHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    verbose = False
    board = ChangeBoard(APPROVAL_WINDOW_S)

    def log_message(self, format: str, *args: object) -> None:
        # One decision line per consult is printed in do_POST instead.
        pass

    def do_GET(self) -> None:  # noqa: N802 - stdlib naming
        if self.path == "/healthz":
            self.reply(200, {"ok": True})
        elif self.path == "/pending":
            self.reply(200, {"pending": self.board.pending()})
        else:
            self.reply(404, {"error": "unknown path"})

    def do_POST(self) -> None:  # noqa: N802 - stdlib naming
        if self.path == "/decide":
            # The side channel: a board member rules on a parked consult.
            body = self.read_json()
            if body is None:
                return
            decided = self.board.decide(body.get("id"), body.get("ruling"), body.get("reason"))
            print(f"[mock] /decide id={body.get('id')} ruling={body.get('ruling')} accepted={decided}", flush=True)
            self.reply(200 if decided else 404, {"decided": body.get("id")} if decided else {"error": "no such parked consult, or the ruling is not approve/deny"})
            return
        envelope = self.read_envelope()
        if envelope is None:
            return
        kind = envelope.get("kind")
        name = envelope.get("name")
        declaration = envelope.get("declaration")
        artifact = envelope.get("artifact")

        if self.path == "/annotate":
            if kind != "annotation":
                self.decide(kind, name, 400, {"error": "the /annotate endpoint answers annotation consults"}, "wrong kind")
                return
            answer, detail = annotate(declaration, artifact)
            if answer is None:
                self.decide(kind, name, 404, {"error": detail}, detail)
            else:
                self.decide(kind, name, 200, {"version": WIRE_VERSION, "answer": answer}, detail)
        elif self.path == "/authorize":
            if kind != "authority":
                self.decide(kind, name, 400, {"error": "the /authorize endpoint answers authority consults"}, "wrong kind")
                return
            answer, detail = authorize(artifact)
            self.decide(kind, name, 200, {"version": WIRE_VERSION, "answer": answer}, detail)
        elif self.path == "/sanitize":
            if kind != "sanitizer":
                wrong = {"error": "the /sanitize endpoint answers sanitizer consults"}
                self.decide(kind, name, 400, wrong, "wrong kind")
                return
            answer, detail = sanitize(artifact)
            if answer is None:
                self.decide(kind, name, 404, {"error": detail}, detail)
            else:
                self.decide(kind, name, 200, {"version": WIRE_VERSION, "answer": answer}, detail)
        elif self.path == "/approve":
            if kind != "authority":
                self.decide(kind, name, 400, {"error": "the /approve endpoint answers authority consults"}, "wrong kind")
                return
            print(f"[mock] /approve kind={kind} name={name} parked; waiting for a ruling", flush=True)
            answer, detail = self.board.consult(declaration, artifact)
            if answer is None:
                self.decide(kind, name, 504, {"error": detail}, detail)
            else:
                self.decide(kind, name, 200, {"version": WIRE_VERSION, "answer": answer}, detail)
        else:
            self.decide(kind, name, 404, {"error": "unknown path"}, "unknown path")

    def read_json(self) -> dict | None:
        length = self.headers.get("Content-Length")
        if length is None or not length.isdigit():
            self.reply(411, {"error": "a request carries a Content-Length"})
            return None
        body = self.rfile.read(int(length))
        try:
            parsed = json.loads(body)
        except ValueError:
            self.reply(400, {"error": "the body is not JSON"})
            return None
        if not isinstance(parsed, dict):
            self.reply(400, {"error": "the body is not an object"})
            return None
        return parsed

    def read_envelope(self) -> dict | None:
        envelope = self.read_json()
        if envelope is None:
            return None
        if envelope.get("version") != WIRE_VERSION:
            self.reply(400, {"error": f"the consult envelope must carry version {WIRE_VERSION}"})
            return None
        if self.verbose:
            print(f"[mock] request {self.path} {json.dumps(envelope, sort_keys=True)}", flush=True)
        return envelope

    def decide(self, kind: object, name: object, status: int, payload: dict, detail: str) -> None:
        print(f"[mock] {self.path} kind={kind} name={name} status={status} {detail}", flush=True)
        self.reply(status, payload)

    def reply(self, status: int, payload: dict) -> None:
        body = json.dumps(payload, separators=(",", ":")).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def main() -> None:
    parser = argparse.ArgumentParser(prog="mock_externals")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8081)
    parser.add_argument("--verbose", action="store_true", help="log every consult envelope in full")
    parser.add_argument(
        "--approval-window", type=float, default=APPROVAL_WINDOW_S, help="seconds a change-board consult waits for a ruling"
    )
    args = parser.parse_args()
    ConsultHandler.verbose = args.verbose
    ConsultHandler.board = ChangeBoard(args.approval_window)
    server = ThreadingHTTPServer((args.host, args.port), ConsultHandler)
    print(
        f"[mock] serving /annotate, /authorize, /approve, /sanitize (+ /pending, /decide) on {args.host}:{args.port}",
        flush=True,
    )
    server.serve_forever()


if __name__ == "__main__":
    main()
