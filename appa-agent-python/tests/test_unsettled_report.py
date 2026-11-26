import json
import threading
from collections.abc import Iterator
from http.server import BaseHTTPRequestHandler, HTTPServer

import pytest
from conftest import decision

from appa_agent_python import AppaError, Session

SCRUBBED = """
version = 2
trust_chain = ["suspicious", "trusted"]

[[tool]]
name = "read_ticket"
delta = { trust = "suspicious" }

[[sanitizer]]
name = "scrub"
on = ["tool_output"]
[sanitizer.permits]
trust = { from = "suspicious", to = "trusted" }
"""

# A bridge is what confines a dispatched result; these tests only check and report, so it
# is never called.
BRIDGE = "http://127.0.0.1:9/cap-0123456789abcdef"


class Scrubber:
    def __init__(self) -> None:
        self.up = False
        self.asked = 0


@pytest.fixture
def scrubber() -> Iterator[tuple[Scrubber, str]]:
    state = Scrubber()

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self) -> None:
            self.rfile.read(int(self.headers["Content-Length"]))
            state.asked += 1
            if state.up:
                body = json.dumps({"version": 1, "answer": {"body": "scrubbed"}}).encode()
                self.send_response(200)
            else:
                body = b"down"
                self.send_response(503)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_: object) -> None:
            pass

    server = HTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    yield state, f"http://127.0.0.1:{server.server_port}/"
    server.shutdown()


def released_under_scrub(url: str) -> Session:
    """A session whose read was released with `scrub` bound to its output."""
    externals = f'timeout_ms = 2000\nmax_body_bytes = 65536\n[sanitizers.scrub]\nurl = "{url}"\n'
    session = Session(SCRUBBED, json.dumps(["read_ticket"]), "read it", bridge_url=BRIDGE, externals_toml=externals)
    blocked = decision(session.check("read_ticket", {"id": "T-1"}))
    assert blocked["kind"] == "blocked", blocked
    # The menu lists accepting the narrowing first, then the sanitizer.
    taken = decision(session.check("execute_remedy_plan", {"offer_id": blocked["offer_ids"][1]}))
    assert taken["kind"] == "control", taken
    assert decision(session.check("read_ticket", {"id": "T-1"}))["kind"] == "allowed"
    return session


def test_a_report_left_on_a_missing_fact_is_reported_again_once_the_fact_is_obtained(
    scrubber: tuple[Scrubber, str],
) -> None:
    state, url = scrubber
    session = released_under_scrub(url)
    unsettled = decision(session.report("raw ticket"))
    assert unsettled["kind"] == "unsettled", unsettled
    state.up = True
    delivered = decision(session.report())
    assert (delivered["kind"], delivered["content"]) == ("delivered", "scrubbed")
    assert state.asked == 2
    with pytest.raises(AppaError):
        session.report("raw ticket")


def test_an_unsettled_report_is_abandoned_and_the_session_goes_on(scrubber: tuple[Scrubber, str]) -> None:
    state, url = scrubber
    session = released_under_scrub(url)
    assert decision(session.report("raw ticket"))["kind"] == "unsettled"
    with pytest.raises(AppaError):
        session.check("read_ticket", {"id": "T-2"})
    session.abandon()
    assert decision(session.check("read_ticket", {"id": "T-2"}))["kind"] == "blocked"
    assert state.asked == 1
