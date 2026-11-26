import json
import socket
import threading
from collections.abc import Iterator
from http.server import BaseHTTPRequestHandler, HTTPServer

import pytest
from conftest import accept_narrowing, decision

from appa_agent_python import Session

ANNOTATED = """
version = 2

[[annotator]]
name = "classifier"
audiences = ["insider"]
hint = "Use insider for data restricted to company readers."

[[tool]]
name = "fetch"
description = "Fetches one URL and returns its body."
parameters = { type = "object", properties = { url = { type = "string" } }, required = ["url"] }
annotator = "classifier"
"""


class _MalformedAnnotator(BaseHTTPRequestHandler):
    def do_POST(self) -> None:
        self.rfile.read(int(self.headers["Content-Length"]))
        body = b'{"version": 1, "answer": 42}'
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_: object) -> None:
        pass


@pytest.fixture
def malformed_annotator() -> Iterator[str]:
    server = HTTPServer(("127.0.0.1", 0), _MalformedAnnotator)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    yield f"http://127.0.0.1:{server.server_port}/"
    server.shutdown()


@pytest.fixture
def unreachable_annotator() -> str:
    probe = socket.socket()
    probe.bind(("127.0.0.1", 0))
    port = probe.getsockname()[1]
    probe.close()
    return f"http://127.0.0.1:{port}/"


def refusal(answered: dict) -> tuple:
    assert answered["kind"] == "blocked", answered
    return answered["deciding"], answered["outcomes"], answered["retry"]


def annotated_fetch(url: str) -> dict:
    externals = f'timeout_ms = 2000\nmax_body_bytes = 65536\n[annotators.classifier]\nurl = "{url}"\n'
    session = Session(ANNOTATED, json.dumps(["fetch"]), "fetch it", externals_toml=externals)
    return decision(session.check("fetch", {"url": "https://example.test"}))


def test_a_block_states_its_consent_and_its_hard_block(session: Session) -> None:
    assert refusal(decision(session.check("read_ticket", {"id": "T-1"}))) == ("consent", ["consent"], None)
    accept_narrowing(session, "read_ticket", {"id": "T-1"})
    session.report("the ticket")
    assert refusal(decision(session.check("memory_write", {}))) == ("hard_block", ["hard_block"], None)


def test_a_missing_fact_says_whether_waiting_or_a_repair_obtains_it(
    unreachable_annotator: str, malformed_annotator: str
) -> None:
    assert refusal(annotated_fetch(unreachable_annotator)) == ("missing_fact", ["missing_fact"], "later")
    assert refusal(annotated_fetch(malformed_annotator)) == ("missing_fact", ["missing_fact"], "after_repair")
