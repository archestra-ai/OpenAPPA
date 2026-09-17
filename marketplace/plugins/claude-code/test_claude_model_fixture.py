import json
import unittest

from claude_model_fixture import event_stream, message

TOOLS = [
    {"name": "Read"},
    {"name": "Write"},
    {"name": "mcp__appa__execute_remedy_plan"},
]


def request(path: str, results=(), *, stream=False):
    messages = [{"role": "user", "content": f"APPA fixture path: {path}"}]
    for index, result in enumerate(results):
        messages.extend(
            [
                {
                    "role": "assistant",
                    "content": [
                        {
                            "type": "tool_use",
                            "id": f"toolu_{index}",
                            "name": "fixture",
                            "input": {},
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
        first = message(request("/tmp/work/out.txt"))
        self.assertEqual(first["content"][0]["name"], "Write")
        self.assertEqual(
            first["content"][0]["input"],
            {"file_path": "/tmp/work/out.txt", "content": "hello"},
        )

        final = message(request("/tmp/work/out.txt", [{"content": "Wrote file"}]))
        self.assertEqual(final["stop_reason"], "end_turn")

    def test_private_scenario_accepts_narrowing_then_attempts_release(self):
        path = "/tmp/work/private.txt"
        first = message(request(path))
        self.assertEqual(first["content"][0]["name"], "Read")

        denied = {
            "content": 'Blocked. Call execute_remedy_plan(offer_id: "o1:fixture")',
            "is_error": True,
        }
        remedy = message(request(path, [denied]))
        self.assertEqual(remedy["content"][0]["name"], "mcp__appa__execute_remedy_plan")
        self.assertEqual(remedy["content"][0]["input"], {"offer_id": "o1:fixture"})

        authorized = message(request(path, [denied, {"content": "Authorized"}]))
        self.assertEqual(authorized["content"][0]["name"], "Read")

        release = message(
            request(path, [denied, {"content": "Authorized"}, {"content": "canary-42"}])
        )
        self.assertEqual(release["content"][0]["name"], "Write")
        self.assertEqual(
            release["content"][0]["input"],
            {"file_path": "/tmp/work/out.txt", "content": "canary-42"},
        )

    def test_stream_encodes_tool_arguments_as_anthropic_events(self):
        answer = message(request("/tmp/work/out.txt", stream=True))
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
