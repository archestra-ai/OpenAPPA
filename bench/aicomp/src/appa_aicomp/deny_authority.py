"""The `operator` authority of an unattended run, served on loopback: no human is present, so every consult is denied."""

import json
import logging
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

logger = logging.getLogger(__name__)

DENY = json.dumps(
    {"version": 1, "answer": {"ruling": "deny", "reason": "no operator is present in this unattended run"}}
).encode()


consults: list[dict[str, object]] = []


class _Handler(BaseHTTPRequestHandler):
    def do_POST(self) -> None:  # noqa: N802
        request = json.loads(self.rfile.read(int(self.headers.get("Content-Length") or 0)))
        consults.append(request.get("artifact") or {})
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(DENY)))
        self.end_headers()
        self.wfile.write(DENY)

    def log_message(self, format: str, *args: object) -> None:
        pass


def serve() -> str:
    """Start the authority on an ephemeral loopback port for this process; return its URL."""
    server = ThreadingHTTPServer(("127.0.0.1", 0), _Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return f"http://127.0.0.1:{server.server_address[1]}/"


def externals_toml(url: str) -> str:
    return f'[authorities.operator]\nurl = "{url}"\n'
