import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import unittest


SCRIPT = Path(__file__).with_name("conversation-trust.py")
SPEC = importlib.util.spec_from_file_location("conversation_trust", SCRIPT)
ANNOTATOR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ANNOTATOR)


def fixture_api(responses):
    """A call answering from recorded Slack Web API payloads by method."""

    def call(method, **params):
        response = responses[method]
        if isinstance(response, Exception):
            raise response
        return response

    return call


def consult(channel_id="C1", audiences=None, **overrides):
    return {
        "version": 1,
        "kind": "annotation",
        "name": ANNOTATOR.NAME,
        "declaration": {"ranks": ["suspicious"], "audiences": audiences or ["@slack:channel/$channel_id", f"@slack:channel/{channel_id}"]},
        "artifact": {"args": {"name": "slack_read_channel", "arguments": {"channel_id": channel_id}}},
        **overrides,
    }


class Established(unittest.TestCase):
    def test_a_conversation_shared_with_another_organization_is_external(self):
        for channel in ({"is_ext_shared": True}, {"is_pending_ext_shared": True}):
            call = fixture_api({"conversations.info": {"ok": True, "channel": channel}})
            self.assertTrue(ANNOTATOR.established_external(call, "C1"), channel)

    def test_a_workspace_conversation_is_not_external(self):
        for channel in ({"is_ext_shared": False, "is_shared": True}, {"is_im": True}):
            call = fixture_api({"conversations.info": {"ok": True, "channel": channel}})
            self.assertFalse(ANNOTATOR.established_external(call, "C1"), channel)

    def test_a_dm_with_a_user_from_another_organization_is_external(self):
        self.assertTrue(ANNOTATOR.established_external(fixture_api({"users.info": {"ok": True, "user": {"is_stranger": True}}}), "U1"))
        self.assertFalse(ANNOTATOR.established_external(fixture_api({"users.info": {"ok": True, "user": {"is_restricted": True}}}), "U1"))

    def test_a_conversation_slack_cannot_answer_for_is_not_external(self):
        for response in ({"ok": False, "error": "ratelimited"}, OSError("timed out"), ["malformed"]):
            self.assertFalse(ANNOTATOR.established_external(fixture_api({"conversations.info": response}), "C1"), response)
        self.assertFalse(ANNOTATOR.established_external(None, "C1"))


class Answer(unittest.TestCase):
    def test_an_external_conversation_enters_suspicious_and_both_keep_its_readers(self):
        self.assertEqual(ANNOTATOR.annotation("C1", True)["delta"], {"trust": "suspicious", "audience": ["@slack:channel/C1"]})
        self.assertEqual(ANNOTATOR.annotation("C1", False)["delta"], {"audience": ["@slack:channel/C1"]})

    def test_a_consult_naming_no_conversation_id_is_refused(self):
        for channel_id in (None, "general", "https://x.slack.com/archives/C1", "$channel_id"):
            with self.assertRaises(ValueError, msg=channel_id):
                ANNOTATOR.channel_of(consult(channel_id=channel_id))


class Program(unittest.TestCase):
    def run_script(self, request):
        return subprocess.run(
            [sys.executable, str(SCRIPT)],
            input=json.dumps(request),
            capture_output=True,
            text=True,
            env={"PATH": "/usr/bin:/bin"},
            check=False,
        )

    def test_without_a_token_the_conversation_keeps_the_session_trust(self):
        result = self.run_script(consult())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["answer"]["delta"], {"audience": ["@slack:channel/C1"]})

    def test_a_mandate_without_the_conversation_readers_exits_2(self):
        self.assertEqual(self.run_script(consult(audiences=["internal"])).returncode, 2)


if __name__ == "__main__":
    unittest.main()
