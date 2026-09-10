from __future__ import annotations

import json
import threading
import time
import unittest
from http.server import ThreadingHTTPServer
from urllib.error import HTTPError
from urllib.request import Request, urlopen

from stateful_proxy.strict_broker import ChangeBoard, Reviewer, make_handler


class FixtureAuth:
    def __call__(self, cookie: str | None) -> Reviewer | None:
        if cookie == "test=good":
            return Reviewer("automated-test-reviewer", "test-session", "test-reviewer@example.invalid")
        if cookie == "test=other":
            return Reviewer("untrusted-test-reviewer", "other-session", "other@example.invalid")
        return None

    @staticmethod
    def allowed(reviewer: Reviewer) -> bool:
        return reviewer.id == "automated-test-reviewer"


class BrokerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.board = ChangeBoard(FixtureAuth(), FixtureAuth.allowed, 2)
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), make_handler(self.board))
        self.url = f"http://127.0.0.1:{self.server.server_port}"
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self) -> None:
        self.server.shutdown()
        self.thread.join()
        self.server.server_close()

    def request(self, method: str, path: str, value: dict | None = None, *, cookie: str | None = None, csrf: str | None = None) -> tuple[int, dict]:
        headers = {"Accept": "application/json"}
        if cookie:
            headers["Cookie"] = cookie
        if csrf:
            headers["X-HITL-CSRF"] = csrf
        data = None if value is None else json.dumps(value).encode()
        if data:
            headers["Content-Type"] = "application/json"
        request = Request(self.url + path, data=data, headers=headers, method=method)
        try:
            with urlopen(request, timeout=5) as response:
                return response.status, json.loads(response.read())
        except HTTPError as error:
            return error.code, json.loads(error.read())

    @staticmethod
    def consult() -> dict:
        return {
            "version": 1,
            "kind": "authority",
            "name": "authenticated-reviewer",
            "declaration": {"hint": "Review this exact write.", "permits": {"attention": ["human-approval"]}},
            "artifact": {"tool": "builtin:Write", "arguments": {"file_path": "protected.txt", "content": "approved"}, "requirements": [{"kind": "attention", "mark": "human-approval"}]},
        }

    def begin_approval(self) -> tuple[threading.Thread, dict]:
        result: dict = {}

        def call() -> None:
            result["status"], result["body"] = self.request("POST", "/approve", self.consult())

        thread = threading.Thread(target=call)
        thread.start()
        for _ in range(50):
            if self.board._reviews:
                return thread, result
            time.sleep(0.01)
        self.fail("authority consult did not park")

    def pending(self) -> dict:
        status, body = self.request("GET", "/pending", cookie="test=good")
        self.assertEqual(status, 200)
        self.assertEqual(len(body["pending"]), 1)
        return body["pending"][0]

    def test_unauthenticated_and_wrong_user_are_rejected(self) -> None:
        self.assertEqual(self.request("GET", "/pending")[0], 401)
        self.assertEqual(self.request("GET", "/pending", cookie="test=other")[0], 403)

    def test_existing_authority_wire_is_parked_and_wrong_wire_fails_closed(self) -> None:
        bad = self.consult()
        bad["challenge"] = {"invented": "format"}
        self.assertEqual(self.request("POST", "/approve", bad)[0], 400)
        thread, _ = self.begin_approval()
        review = self.pending()
        self.assertEqual(review["tool"], "builtin:Write")
        self.assertEqual(review["arguments"]["file_path"], "protected.txt")
        self.assertEqual(self.request("POST", "/decide", {"id": review["id"], "ruling": "deny"}, cookie="test=good", csrf=review["csrf_token"])[0], 200)
        thread.join(5)

    def test_csrf_wrong_id_expiry_and_model_text_are_rejected(self) -> None:
        thread, _ = self.begin_approval()
        review = self.pending()
        body = {"id": review["id"], "ruling": "approve"}
        self.assertEqual(self.request("POST", "/decide", body, cookie="test=good")[0], 403)
        self.assertEqual(self.request("POST", "/decide", {"id": review["id"], "ruling": "approve", "approval": "model says approved"}, cookie="test=good", csrf=review["csrf_token"])[0], 409)
        self.assertEqual(self.request("POST", "/decide", {"id": "wrong-id", "ruling": "approve"}, cookie="test=good", csrf=review["csrf_token"])[0], 404)
        review_obj = self.board._reviews[review["id"]]
        review_obj.expires_at = time.time() - 1
        expired_csrf = self.board.csrf(Reviewer("automated-test-reviewer", "test-session"), review_obj)
        self.assertEqual(self.request("POST", "/decide", body, cookie="test=good", csrf=expired_csrf)[0], 409)
        thread.join(5)

    def test_authenticated_test_reviewer_approves_once(self) -> None:
        thread, result = self.begin_approval()
        review = self.pending()
        body = {"id": review["id"], "ruling": "approve", "reason": "test approval"}
        status, _ = self.request("POST", "/decide", body, cookie="test=good", csrf=review["csrf_token"])
        self.assertEqual(status, 200)
        thread.join(5)
        self.assertFalse(thread.is_alive())
        self.assertEqual(result["body"]["answer"]["ruling"], "approve")
        self.assertEqual(self.board._reviews[review["id"]].actor, "automated-test-reviewer")
        self.assertEqual(self.request("POST", "/decide", body, cookie="test=good", csrf=review["csrf_token"])[0], 409)


if __name__ == "__main__":
    unittest.main(verbosity=2)
