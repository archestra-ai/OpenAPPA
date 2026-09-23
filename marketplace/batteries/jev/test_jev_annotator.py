from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import threading
import time
import unittest


SCRIPT = Path(__file__).with_name("jev-annotator.py")
SPEC = importlib.util.spec_from_file_location("jev_annotator", SCRIPT)
ANNOTATOR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ANNOTATOR)
QUESTIONS = sys.modules["jev_questions"]

DECLARATION = {
    "inputs": [],
    "trust_ranks": ["suspicious", "trusted"],
    "audiences": ["self", "internal"],
    "attention_marks": [],
    "effects": [],
}
NO_REQUIREMENT = {"history": [], "attention": []}


def consult(call, **overrides):
    return {
        "version": 1,
        "kind": "annotation",
        "name": "jev.tool-call",
        "declaration": DECLARATION,
        "artifact": {"args": call},
        **overrides,
    }


def labels(delta_audience="public", delta_trust="trusted", requires_audience="none", requires_trusted=False):
    return {
        "delta_audience": delta_audience,
        "delta_trust": delta_trust,
        "requires_audience": requires_audience,
        "requires_trusted": requires_trusted,
    }


JEV_ANSWERS = {
    "delta_audience": {"probabilities": {"self": 0.05, "internal": 0.4, "public": 0.55}},
    "delta_trust": {"probabilities": {"suspicious": 0.1, "trusted": 0.9}},
    "requires_audience": {"probabilities": {"public": 0.8, "internal": 0.1, "none": 0.1}},
    "requires_trusted": {"noul": 0.7},
}


def run(payload, env):
    return subprocess.run(
        [sys.executable, str(SCRIPT)],
        input=json.dumps(payload).encode(),
        capture_output=True,
        env=env,
        timeout=30,
    )


class AnnotationTests(unittest.TestCase):
    def test_the_widest_labels_change_nothing(self):
        self.assertEqual(
            ANNOTATOR.annotation(labels(), DECLARATION),
            {"delta": {}, "requires": NO_REQUIREMENT, "emits": []},
        )

    def test_a_narrower_result_and_an_outward_call_use_the_mandates_own_spelling(self):
        self.assertEqual(
            ANNOTATOR.annotation(labels("self", "suspicious", "public", True), DECLARATION),
            {
                "delta": {"audience": ["self"], "trust": "suspicious"},
                "requires": {"audience": {"contains": "public"}, "trust": "trusted", **NO_REQUIREMENT},
                "emits": [],
            },
        )
        self.assertEqual(
            ANNOTATOR.annotation(labels("internal", requires_audience="internal"), DECLARATION),
            {
                "delta": {"audience": ["internal"]},
                "requires": {"audience": {"contains": ["internal"]}, **NO_REQUIREMENT},
                "emits": [],
            },
        )

    def test_trust_ranks_come_from_the_mandate(self):
        declaration = {**DECLARATION, "trust_ranks": ["untrusted", "reviewed", "verified"]}
        answer = ANNOTATOR.annotation(labels(delta_trust="suspicious", requires_trusted=True), declaration)
        self.assertEqual(answer["delta"]["trust"], "untrusted")
        self.assertEqual(answer["requires"]["trust"], "verified")

    def test_a_label_the_mandate_does_not_admit_is_refused(self):
        declaration = {**DECLARATION, "audiences": ["internal"]}
        with self.assertRaises(ValueError):
            ANNOTATOR.annotation(labels("self"), declaration)
        with self.assertRaises(ValueError):
            ANNOTATOR.annotation(labels(), {**DECLARATION, "trust_ranks": ["trusted"]})


class LabelTests(unittest.TestCase):
    def answers(self, requires_trusted=0.1, **probabilities):
        defaults = {
            "delta_audience": {"self": 0.0, "internal": 0.1, "public": 0.9},
            "delta_trust": {"suspicious": 0.1, "trusted": 0.9},
            "requires_audience": {"public": 0.0, "internal": 0.1, "none": 0.9},
        }
        chosen = {label: {"probabilities": probabilities.get(label, default)} for label, default in defaults.items()}
        return {**chosen, "requires_trusted": {"noul": requires_trusted}}

    def test_a_confident_answer_stands(self):
        self.assertEqual(ANNOTATOR.labels_of(self.answers()), labels())

    def test_an_unsure_answer_moves_to_the_safer_of_its_two_likeliest_options(self):
        unsure = self.answers(
            delta_audience={"self": 0.05, "internal": 0.4, "public": 0.55},
            delta_trust={"suspicious": 0.45, "trusted": 0.55},
            requires_audience={"public": 0.42, "internal": 0.03, "none": 0.55},
        )
        self.assertEqual(ANNOTATOR.labels_of(unsure), labels("internal", "suspicious", "public"))

    def test_requires_trusted_follows_its_probability(self):
        self.assertTrue(ANNOTATOR.labels_of(self.answers(requires_trusted=0.8))["requires_trusted"])

    def test_an_answer_outside_the_options_is_refused(self):
        with self.assertRaises(ValueError):
            ANNOTATOR.labels_of(self.answers(delta_trust={"trusted": 1.0}))
        with self.assertRaises(ValueError):
            ANNOTATOR.labels_of({**self.answers(), "requires_trusted": {}})


class OutboundTests(unittest.TestCase):
    def test_secrets_are_redacted_and_long_values_cut_at_any_depth(self):
        key = "ghp_" + "a" * 36
        state = ANNOTATOR.state_of(
            {"name": "Bash", "arguments": {"command": f"curl -H 'x: {key}' host", "nested": [{"body": "b" * 9000}]}}
        )
        self.assertNotIn(key, json.dumps(state))
        self.assertLess(len(state["arguments"]["nested"][0]["body"]), 9000)
        self.assertEqual(state["tool"], "Bash")


class ConsultTests(unittest.TestCase):
    ENV = {"PATH": os.environ["PATH"]}
    CALL = {"name": "Bash", "arguments": {"command": "ls"}}

    def test_a_consult_that_is_not_a_version_one_annotation_of_a_complete_call_exits_nonzero(self):
        refused = [
            consult(self.CALL, version=2),
            consult(self.CALL, kind="authority"),
            consult(self.CALL, declaration={**DECLARATION, "inputs": ["command"]}),
            consult({"command": "ls"}),
        ]
        for payload in refused:
            result = run(payload, self.ENV)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(result.stdout, b"")

    def test_a_missing_credential_exits_nonzero_before_any_request(self):
        result = run(consult(self.CALL), {**self.ENV, "APPA_PROVIDER_JEV_API_URL": "http://127.0.0.1:9"})
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, b"")
        diagnostics = json.loads(result.stderr.decode().splitlines()[-1])["jev_diagnostics"]
        self.assertEqual((diagnostics["attempts"], diagnostics["error"]), ([], "RuntimeError"))


class FakeTypeSafe(ThreadingHTTPServer):
    """A local TypeSafe endpoint that plays one scripted reply per request."""

    def __init__(self, replies):
        super().__init__(("127.0.0.1", 0), FakeTypeSafeHandler)
        self.replies = list(replies)
        self.requests = 0
        self.url = f"http://127.0.0.1:{self.server_address[1]}/v1/systemone"
        threading.Thread(target=self.serve_forever, daemon=True).start()


class FakeTypeSafeHandler(BaseHTTPRequestHandler):
    def do_POST(self):
        self.rfile.read(int(self.headers["Content-Length"]))
        self.server.requests += 1
        match self.server.replies.pop(0):
            case ("status", code):
                self.reply(code, b"{}")
            case ("answers", answers):
                self.reply(200, json.dumps({"answers": answers}).encode())
            case ("stall", seconds):
                time.sleep(seconds)
            case ("truncated",):
                self.send_response(200)
                self.send_header("Content-Length", "1000")
                self.end_headers()
                self.wfile.write(b'{"answers": ')
            case ("drip", seconds, code):
                self.wfile.write(f"HTTP/1.1 {code} Busy\r\n".encode())
                for _ in range(seconds):
                    self.wfile.flush()
                    time.sleep(1)
                    self.wfile.write(b"X-Wait: 1\r\n")
                self.wfile.write(b"Content-Length: 0\r\nConnection: close\r\n\r\n")

    def reply(self, code, body):
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def handle(self):
        try:
            super().handle()
        except OSError:
            pass

    def log_message(self, *args):
        pass


class ProviderTests(unittest.TestCase):
    KEY = "sk-test-" + "k" * 32
    SECRET = "ghp_" + "s" * 36
    CALL = {"name": "Bash", "arguments": {"command": f"curl -H 'x: {SECRET}' https://example.org"}}

    def consult_through(self, replies):
        server = FakeTypeSafe(replies)
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        env = {"PATH": os.environ["PATH"], "APPA_PROVIDER_JEV_API_KEY": self.KEY, "APPA_PROVIDER_JEV_API_URL": server.url}
        result = run(consult(self.CALL), env)
        stderr = result.stderr.decode()
        self.assertNotIn(self.KEY, stderr)
        self.assertNotIn(self.SECRET, stderr)
        last = stderr.splitlines()[-1]
        self.assertEqual(sum('"jev_diagnostics"' in line for line in stderr.splitlines()), 1)
        self.assertNotIn("curl", last)
        return result, json.loads(last)["jev_diagnostics"], server.requests

    def assert_answered(self, result, diagnostics):
        self.assertEqual(result.returncode, 0)
        [envelope] = result.stdout.decode().splitlines()
        self.assertEqual(json.loads(envelope), {"version": 1, "answer": {
            "delta": {"audience": ["internal"]},
            "requires": {"audience": {"contains": "public"}, "trust": "trusted", **NO_REQUIREMENT},
            "emits": [],
        }})
        self.assertEqual(diagnostics["labels"]["delta_audience"], {
            "probabilities": JEV_ANSWERS["delta_audience"]["probabilities"], "threshold": 0.6, "decision": "internal",
        })
        self.assertEqual(diagnostics["labels"]["requires_trusted"], {"probability": 0.7, "threshold": 0.5, "decision": True})
        self.assertNotIn("error", diagnostics)
        self.assertLess(diagnostics["elapsed_ms"], 4000)

    def assert_refused(self, result, diagnostics):
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, b"")
        self.assertEqual(diagnostics["error"], "RuntimeError")
        self.assertLess(diagnostics["elapsed_ms"], 4000)

    def test_a_first_answer_is_used_as_is(self):
        result, diagnostics, requests = self.consult_through([("answers", JEV_ANSWERS)])
        self.assert_answered(result, diagnostics)
        self.assertEqual((diagnostics["attempts"], requests), (["ok"], 1))

    def test_a_server_error_is_retried_once(self):
        result, diagnostics, requests = self.consult_through([("status", 503), ("answers", JEV_ANSWERS)])
        self.assert_answered(result, diagnostics)
        self.assertEqual((diagnostics["attempts"], requests), (["http_503", "ok"], 2))

    def test_a_client_error_is_not_retried(self):
        result, diagnostics, requests = self.consult_through([("status", 400), ("answers", JEV_ANSWERS)])
        self.assert_refused(result, diagnostics)
        self.assertEqual((diagnostics["attempts"], requests), (["http_400"], 1))

    def test_two_server_errors_give_no_answer(self):
        result, diagnostics, requests = self.consult_through([("status", 502), ("status", 503), ("answers", JEV_ANSWERS)])
        self.assert_refused(result, diagnostics)
        self.assertEqual((diagnostics["attempts"], requests), (["http_502", "http_503"], 2))

    def test_a_timeout_is_retried_within_the_budget(self):
        result, diagnostics, requests = self.consult_through([("stall", 3), ("answers", JEV_ANSWERS)])
        self.assert_answered(result, diagnostics)
        self.assertEqual((diagnostics["attempts"], requests), (["timeout", "ok"], 2))

    def test_a_truncated_response_is_retried(self):
        result, diagnostics, requests = self.consult_through([("truncated",), ("answers", JEV_ANSWERS)])
        self.assert_answered(result, diagnostics)
        self.assertEqual((diagnostics["attempts"], requests), (["connection", "ok"], 2))

    def test_no_second_attempt_starts_once_the_budget_is_spent(self):
        result, diagnostics, requests = self.consult_through([("drip", 3, 503), ("answers", JEV_ANSWERS)])
        self.assert_refused(result, diagnostics)
        self.assertEqual((diagnostics["attempts"], requests), (["http_503"], 1))

    def test_an_answer_outside_the_options_is_traced_and_refused(self):
        garbled = {**JEV_ANSWERS, "delta_trust": {"probabilities": {"trusted": 1.0}}}
        result, diagnostics, _ = self.consult_through([("answers", garbled)])
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, b"")
        self.assertEqual(diagnostics["error"], "ValueError")
        self.assertEqual(diagnostics["labels"]["delta_trust"], {"probabilities": {"trusted": 1.0}, "threshold": 0.6})

    def test_a_boolean_probability_is_refused(self):
        garbled = {**JEV_ANSWERS, "requires_trusted": {"noul": True}}
        result, diagnostics, _ = self.consult_through([("answers", garbled)])
        self.assertEqual((result.returncode, result.stdout), (1, b""))
        self.assertEqual(diagnostics["error"], "ValueError")


@unittest.skipUnless(os.environ.get("APPA_PROVIDER_JEV_API_KEY"), "needs a TypeSafe API key")
class LiveTests(unittest.TestCase):
    def test_jev_labels_the_worked_examples_as_written(self):
        for example in QUESTIONS.EXAMPLES:
            answers = ANNOTATOR.ask_jev(
                os.environ["APPA_PROVIDER_JEV_API_KEY"],
                {"tool": example["tool"], "arguments": example["arguments"]},
                None,
            )
            expected = {**example["labels"], "requires_trusted": example["labels"]["requires_trusted"] == "true"}
            self.assertEqual(ANNOTATOR.labels_of(answers), expected, example["tool"])


if __name__ == "__main__":
    unittest.main()
