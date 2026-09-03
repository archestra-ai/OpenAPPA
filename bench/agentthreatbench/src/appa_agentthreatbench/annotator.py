"""Per-sample trusted annotator fixture for Data Exfiltration."""

import hashlib
import json
import secrets
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from threading import Lock, Thread
from typing import Any

MAX_REQUEST_BYTES = 64 * 1024


def minimized_customer_acl(customer_db: list[dict[str, Any]]) -> list[dict[str, str]]:
    """Remove issue text, sensitive fields, and benchmark-only metadata."""
    return [
        {
            "name": str(customer.get("name", "")),
            "id": str(customer.get("id", "")),
            "email": str(customer.get("email", "")),
        }
        for customer in customer_db
    ]


def mandate_readers(customer_db: list[dict[str, Any]]) -> tuple[str, ...]:
    """Every literal reader an annotation of this sample may name: each customer's
    id and email, in the fixture's reader spelling. Pure in the sample's customer
    directory, so the manifest computes it without serving a fixture."""
    minimized = minimized_customer_acl(customer_db)
    values = [value for customer in minimized for value in (customer["id"], customer["email"])]
    return tuple(_literal_readers(values))


def _literal_readers(values: list[str]) -> list[str]:
    return sorted({value.strip().lower() for value in values if value.strip()})


def annotator_fixture_digest(customer_db: list[dict[str, Any]]) -> str:
    payload = {"customer_acl": minimized_customer_acl(customer_db)}
    encoded = json.dumps(payload, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(encoded).hexdigest()


class AnnotatorFixture:
    """Serve and audit complete per-call annotations for the Data Exfiltration tools.

    One annotator answers each covered tool: the customer directory establishes a
    record's output audience, and each recipient directory demands the reader the
    message names and emits that tool's declared effect kind.
    """

    def __init__(self, customer_db: list[dict[str, Any]]) -> None:
        self.customer_db = minimized_customer_acl(customer_db)
        self.digest = annotator_fixture_digest(self.customer_db)
        self.requests: list[dict[str, object]] = []
        self._lock = Lock()
        self._capability = secrets.token_urlsafe(24)
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
                if self.path != f"/{fixture._capability}":
                    self.send_error(404)
                    return
                length = int(self.headers.get("Content-Length", "0"))
                if length <= 0 or length > MAX_REQUEST_BYTES:
                    self.send_error(413)
                    return
                try:
                    request = json.loads(self.rfile.read(length))
                    annotation = fixture.annotate(request)
                except (KeyError, TypeError, ValueError, json.JSONDecodeError):
                    self.send_error(400)
                    return
                body = json.dumps({"version": 1, "answer": annotation}, separators=(",", ":")).encode()
                with fixture._lock:
                    fixture.requests.append({"request": request, "annotation": annotation})
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.send_header("Connection", "close")
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, format: str, *args: object) -> None:
                return

        self._server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self._server.daemon_threads = True
        self._thread = Thread(target=self._server.serve_forever, name="appa-agentthreatbench-annotator", daemon=True)
        self._thread.start()

    @property
    def url(self) -> str:
        host, port = self._server.server_address
        return f"http://{host}:{port}/{self._capability}"

    def mandate_readers(self) -> tuple[str, ...]:
        """The policy closes each annotator's `audiences` mandate to exactly this set,
        so a recipient outside the directory makes the produced annotation
        inadmissible."""
        return mandate_readers(self.customer_db)

    def annotate(self, request: object) -> dict[str, object]:
        """The complete annotation this consult asks for. The consult carries no tool
        name, so the annotator name and its own mapped input are the whole key."""
        if not isinstance(request, dict) or not {"version", "kind", "name", "artifact"} <= set(request):
            raise ValueError("invalid annotation consult")
        artifact = request["artifact"]
        args = artifact.get("args") if isinstance(artifact, dict) else None
        if (
            request["version"] != 1
            or request["kind"] != "annotation"
            or not isinstance(args, dict)
            or not isinstance(args.get("subject"), str)
        ):
            raise ValueError("invalid annotation consult version or args")
        subject = args["subject"]
        match request["name"]:
            case "customer-acl":
                return {
                    "delta": {"audience": self._customer_readers(subject)},
                    "requires": {"history": [], "attention": []},
                    "emits": [],
                }
            case "message-recipient-members":
                return self._recipient_annotation(subject, "message.sent")
            case "response-recipient-members":
                return self._recipient_annotation(subject, "response.delivered")
            case _:
                raise ValueError("undeclared annotator")

    @staticmethod
    def _recipient_annotation(recipient: str, effect: str) -> dict[str, object]:
        """The recipient directory demands the named reader and emits the sending
        tool's one declared effect kind; the send itself narrows no label."""
        return {
            "delta": {},
            "requires": {
                "audience": {"contains": _literal_readers([recipient])},
                "history": [],
                "attention": [],
            },
            "emits": [effect],
        }

    def snapshot(self) -> list[dict[str, object]]:
        with self._lock:
            return list(self.requests)

    def close(self) -> None:
        self._server.shutdown()
        self._server.server_close()
        self._thread.join(timeout=5)

    def _customer_readers(self, query: str) -> list[str]:
        # Keep this selection predicate identical to AgentThreatBench's pinned
        # lookup_customer tool. In particular, the tool does not trim queries,
        # search email addresses, fuzzy-match IDs, or normalize ID case. The
        # first matching record is returned.
        selected = next(
            (
                customer
                for customer in self.customer_db
                if query.lower() in customer.get("name", "").lower() or query == customer.get("id", "")
            ),
            None,
        )
        if selected is None:
            return []
        return _literal_readers([str(selected.get("id", "")), str(selected.get("email", ""))])
