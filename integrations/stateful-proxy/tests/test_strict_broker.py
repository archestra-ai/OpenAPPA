"""Retained regressions for the deployed strict review broker, without model calls."""
import copy
import json
from pathlib import Path
import tempfile
import threading
import unittest
from http.server import ThreadingHTTPServer

from stateful_proxy.strict_broker import BrokerError, ChangeBoard, Reviewer, make_handler
import strict_test_support


class StrictBrokerTests(strict_test_support.BrokerTests):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.audit = Path(self.directory.name) / "audit.jsonl"
        self.board = ChangeBoard(strict_test_support.FixtureAuth(), strict_test_support.FixtureAuth.allowed, 2,
                                 audit_file=self.audit, require_review_context=True)
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), make_handler(self.board))
        self.url = f"http://127.0.0.1:{self.server.server_port}"
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self):
        super().tearDown()
        self.directory.cleanup()

    @staticmethod
    def consult():
        value = strict_test_support.BrokerTests.consult()
        value["artifact"]["logical_action_digest"] = "a" * 64
        value["artifact"]["review_scope"] = {
            "root_id": "fixture-root", "offer_id": "fixture-offer",
            "opening_policy_fingerprint": "b" * 64,
        }
        return value

    def test_missing_scope_is_refused(self):
        value = self.consult()
        del value["artifact"]["review_scope"]
        self.assertEqual(self.request("POST", "/approve", value)[0], 400)

    def test_malformed_scope_is_refused(self):
        value = self.consult()
        value["artifact"]["review_scope"]["opening_policy_fingerprint"] = "not-a-fingerprint"
        self.assertEqual(self.request("POST", "/approve", value)[0], 400)

    def test_scope_and_action_changes_change_review_digest(self):
        initial = self.board.register(self.consult())
        for key in ("root_id", "offer_id", "opening_policy_fingerprint", "child_id"):
            value = copy.deepcopy(self.consult())
            value["artifact"]["review_scope"][key] = "c" * 64 if key == "opening_policy_fingerprint" else "other"
            self.assertNotEqual(initial.action_id, self.board.register(value).action_id)
        value = self.consult()
        value["artifact"]["logical_action_digest"] = "d" * 64
        self.assertNotEqual(initial.action_id, self.board.register(value).action_id)

    def test_audit_failure_does_not_grant(self):
        def fail(_record):
            raise OSError("test disk failure")
        board = ChangeBoard(strict_test_support.FixtureAuth(), strict_test_support.FixtureAuth.allowed, 2, audit_append=fail,
                            require_review_context=True)
        review = board.register(self.consult())
        actor = Reviewer("automated-test-reviewer", "test-session")
        with self.assertRaisesRegex(BrokerError, "audit write failed"):
            board.decide(actor, {"id": review.id, "ruling": "approve"}, board.csrf(actor, review))
        self.assertEqual(review.state, "pending")
        self.assertIsNone(review.ruling)

    def test_success_audit_contains_bound_scope_not_credentials(self):
        review = self.board.register(self.consult())
        actor = Reviewer("automated-test-reviewer", "test-session")
        token = self.board.csrf(actor, review)
        self.board.decide(actor, {"id": review.id, "ruling": "approve"}, token)
        record = json.loads(self.audit.read_text())
        self.assertEqual(record["review_scope"], self.consult()["artifact"]["review_scope"])
        self.assertEqual(record["logical_action_digest"], "a" * 64)
        self.assertEqual(record["authenticated_reviewer_id"], actor.id)
        self.assertNotIn(token, self.audit.read_text())
        self.assertEqual(self.audit.stat().st_mode & 0o777, 0o600)


if __name__ == "__main__":
    unittest.main()
