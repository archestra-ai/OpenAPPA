import json
import unittest

from claude_model_fixture import UPLOAD_HOST, event_stream, message

TOOLS = [
    {"name": "Read"},
    {"name": "Write"},
    {"name": "Bash"},
    {"name": "mcp__appa__execute_remedy_plan"},
]


def request(scenario: str, path: str, results=(), *, stream=False):
    """A conversation in which each result answers the call before it. A
    result's `call` is the call the model made; by default an empty one."""
    messages = [
        {
            "role": "user",
            "content": f"APPA fixture scenario: {scenario}\nAPPA fixture path: {path}",
        }
    ]
    for index, result in enumerate(results):
        result = dict(result)
        call = result.pop("call", {"name": "fixture", "input": {}})
        messages.extend(
            [
                {
                    "role": "assistant",
                    "content": [
                        {
                            "type": "tool_use",
                            "id": f"toolu_{index}",
                            **call,
                        }
                    ],
                },
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "tool_result",
                            "tool_use_id": f"toolu_{index}",
                            **result,
                        }
                    ],
                },
            ]
        )
    return {"model": "test", "messages": messages, "tools": TOOLS, "stream": stream}


class ModelFixtureTests(unittest.TestCase):
    def test_public_scenario_writes_then_stops(self):
        first = message(request("public", "/tmp/work/out.txt"))
        self.assertEqual(first["content"][0]["name"], "Write")
        self.assertEqual(
            first["content"][0]["input"],
            {"file_path": "/tmp/work/out.txt", "content": "hello"},
        )

        final = message(request("public", "/tmp/work/out.txt", [{"content": "Wrote file"}]))
        self.assertEqual(final["stop_reason"], "end_turn")

    def test_private_scenario_accepts_narrowing_then_attempts_release(self):
        path = "/tmp/work/private.txt"
        first = message(request("private", path))
        self.assertEqual(first["content"][0]["name"], "Read")

        denied = {
            "content": 'Blocked. Call execute_remedy_plan(offer_id: "o1:fixture")',
            "is_error": True,
        }
        remedy = message(request("private", path, [denied]))
        self.assertEqual(remedy["content"][0]["name"], "mcp__appa__execute_remedy_plan")
        self.assertEqual(remedy["content"][0]["input"], {"offer_id": "o1:fixture"})

        authorized = message(request("private", path, [denied, {"content": "Authorized"}]))
        self.assertEqual(authorized["content"][0]["name"], "Read")

        release = message(
            request(
                "private",
                path,
                [denied, {"content": "Authorized"}, {"content": "canary-42"}],
            )
        )
        self.assertEqual(release["content"][0]["name"], "Write")
        self.assertEqual(
            release["content"][0]["input"],
            {"file_path": "/tmp/work/out.txt", "content": "canary-42"},
        )

    def test_shell_scenario_copies_accepts_narrowing_then_attempts_upload(self):
        path = "/tmp/work/.env"
        first = message(request("shell", path))
        copy = first["content"][0]
        self.assertEqual(copy["name"], "Bash")
        self.assertEqual(copy["input"]["command"], "cp .env copy.txt")
        self.assertEqual(copy["input"]["input_files"], ["/tmp/work/.env"])
        self.assertEqual(copy["input"]["output_files"], ["/tmp/work/copy.txt"])
        self.assertEqual(copy["input"]["network_hosts"], [])

        # Claude Code keeps the command but drops the declared lists from history.
        copied = {"name": "Bash", "input": {"command": "cp .env copy.txt"}}
        denied = {
            "call": copied,
            "content": 'Blocked. Call mcp__appa__execute_remedy_plan(offer_id: "o1")',
            "is_error": True,
        }
        remedy = message(request("shell", path, [denied]))
        self.assertEqual(remedy["content"][0]["name"], "mcp__appa__execute_remedy_plan")
        self.assertEqual(remedy["content"][0]["input"], {"offer_id": "o1"})

        authorized = {"content": "[appa] Authorized. Call the Bash tool"}
        again = message(request("shell", path, [denied, authorized]))
        self.assertEqual(again["content"][0]["input"], copy["input"])

        ran = {"call": copied, "content": "(Bash completed with no output)", "is_error": False}
        upload = message(request("shell", path, [denied, authorized, ran]))["content"][0]
        self.assertEqual(upload["name"], "Bash")
        self.assertIn(UPLOAD_HOST, upload["input"]["command"])
        self.assertEqual(upload["input"]["input_files"], ["/tmp/work/copy.txt"])
        self.assertEqual(upload["input"]["network_hosts"], [UPLOAD_HOST])

        uploading = {"name": "Bash", "input": {"command": upload["input"]["command"]}}
        refused = {
            "call": uploading,
            "content": 'Blocked. Call mcp__appa__execute_remedy_plan(offer_id: "o2")',
            "is_error": True,
        }
        accepted = message(request("shell", path, [denied, authorized, ran, refused]))
        self.assertEqual(accepted["content"][0]["input"], {"offer_id": "o2"})

        unanswered = {"content": "authority hitl gave no answer; the offer stands"}
        final = message(request("shell", path, [denied, authorized, ran, refused, unanswered]))
        self.assertEqual(final["stop_reason"], "end_turn")

        released = message(request("shell", path, [denied, authorized, ran, refused, authorized]))
        self.assertEqual(released["content"][0]["input"], upload["input"])

    def test_a_repeated_block_of_the_copy_ends_the_turn(self):
        path = "/tmp/work/.env"
        copied = {"name": "Bash", "input": {"command": "cp .env copy.txt"}}
        blocked = {
            "call": copied,
            "content": 'Blocked. Call mcp__appa__execute_remedy_plan(offer_id: "o1")',
            "is_error": True,
        }
        authorized = {"content": "[appa] Authorized. Call the Bash tool"}
        final = message(request("shell", path, [blocked, authorized, blocked]))
        self.assertEqual(final["stop_reason"], "end_turn")

    def test_replies_carry_distinct_message_ids(self):
        first = message(request("public", "/tmp/work/out.txt"))
        second = message(request("public", "/tmp/work/out.txt"))
        self.assertNotEqual(first["id"], second["id"])

    def test_stream_encodes_tool_arguments_as_anthropic_events(self):
        answer = message(request("public", "/tmp/work/out.txt", stream=True))
        stream = event_stream(answer).decode()
        events = [
            line.removeprefix("data: ")
            for line in stream.splitlines()
            if line.startswith("data: ")
        ]
        payloads = [json.loads(event) for event in events]

        deltas = [payload.get("delta", {}) for payload in payloads]
        arguments = [
            delta["partial_json"]
            for delta in deltas
            if delta.get("type") == "input_json_delta"
        ]
        self.assertEqual(
            json.loads(arguments[0]),
            {"file_path": "/tmp/work/out.txt", "content": "hello"},
        )


if __name__ == "__main__":
    unittest.main()
