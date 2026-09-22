import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
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
