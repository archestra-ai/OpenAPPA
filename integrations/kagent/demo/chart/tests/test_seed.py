"""Offline checks for the website-only demo seed and upgrade cleanup."""

import importlib.util
import json
import os
from pathlib import Path
import re
import unittest
from unittest.mock import patch


CHART = Path(__file__).resolve().parents[1]
with patch.dict(os.environ, {"KAGENT_CONTROLLER_URL": "http://controller", "SEED_AGENT_REF": "kagent/cluster-ops"}):
    spec = importlib.util.spec_from_file_location("seed", CHART / "files/seed.py")
    seed = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(seed)


class SeedTests(unittest.TestCase):
    def setUp(self):
        self.fixtures = json.loads((CHART / "files/showcase-sessions.json").read_text())
        self.calls = []
        self.sessions = {seed.stable(f"session/{key}"): [{"id": f"old-task-{key}"}] for key in seed.REPLACED_KEYS}
        for key in ("change-board-approve", "pods"):
            self.sessions[seed.stable(f"website/session/{key}")] = [{"id": f"removed-website-task-{key}"}]
        self.sessions["user-created-chat"] = []

    def api(self, method, path, body=None):
        self.calls.append((method, path, body))
        if method == "POST" and path == "/sessions":
            self.assertEqual(body["agent_ref"], "kagent/cluster-ops")
            self.sessions.setdefault(body["id"], [])
            return 201, {}
        if method == "GET" and path.endswith("/tasks"):
            return 200, {"data": self.sessions[path.split("/")[2]]}
        if method == "POST" and path == "/tasks":
            tasks = self.sessions[body["contextId"]]
            tasks[:] = [task for task in tasks if task["id"] != body["id"]]
            tasks.append(body)
            return 201, {}
        if method == "GET" and path == "/sessions":
            return 200, {"data": [{"id": key} for key in self.sessions]}
        if method == "DELETE" and path.startswith("/tasks/"):
            task_id = path.split("/")[2]
            for tasks in self.sessions.values():
                tasks[:] = [task for task in tasks if task["id"] != task_id]
            return 204, {}
        if method == "DELETE":
            self.assertEqual(self.sessions[path.split("/")[2]], [])
            del self.sessions[path.split("/")[2]]
            return 200, {}
        self.fail(f"Unexpected API call: {method} {path}")

    def run_seed(self, api=None):
        with patch.object(seed, "FIXTURE", CHART / "files/showcase-sessions.json"), \
             patch.object(seed, "wait_for_agent"), patch.object(seed, "api", side_effect=api or self.api):
            seed.main()

    def test_fixtures_match_website_prompts_and_order(self):
        self.assertEqual(seed.ORDER, ["exfil", "ingress", "hitl", "delegation", "annotator"])
        self.assertEqual(set(self.fixtures), set(seed.ORDER))
        website = (CHART.parents[3] / "website/content/docs/kagent.md").read_text()
        scenarios = website.split("## Demonstration scenarios", 1)[1].split("## Protect existing agents", 1)[0]
        prompts = re.findall(r"```text\n(.*?)\n```", scenarios, re.DOTALL)
        names = re.findall(r"#### ([^\n]+)", scenarios)
        recorded = []
        for key in seed.ORDER:
            case = self.fixtures[key]
            self.assertEqual(case["name"], names[seed.ORDER.index(key)])
            self.assertTrue(case["tasks"])
            texts = {
                part.get("text")
                for task in case["tasks"] for message in task.get("history", [])
                if message.get("role") == "user"
                for part in message.get("parts", []) if part.get("kind") == "text"
            }
            case_prompts = prompts[4:6] if key == "annotator" else [prompts[seed.ORDER.index(key)]]
            for prompt in case_prompts:
                self.assertIn(prompt, texts, key)
                recorded.append(prompt)
        self.assertEqual(recorded, prompts)

    def test_seed_replaces_only_owned_chats_and_is_idempotent(self):
        self.run_seed()
        expected = {seed.stable(f"website/session/{key}") for key in seed.ORDER}
        self.assertEqual(set(self.sessions), expected | {"user-created-chat"})
        created = [body["id"] for method, path, body in self.calls if method == "POST" and path == "/sessions"]
        self.assertEqual(created, [seed.stable(f"website/session/{key}") for key in reversed(seed.ORDER)])
        self.calls.clear()
        self.run_seed()
        self.assertFalse(any(method == "DELETE" or (method == "POST" and path == "/tasks") for method, path, _ in self.calls))

    def test_failed_capture_replay_preserves_existing_chats(self):
        def fail_task(method, path, body=None):
            return (500, {}) if path == "/tasks" else self.api(method, path, body)

        with self.assertRaises(SystemExit):
            self.run_seed(fail_task)
        self.assertFalse(any(method == "DELETE" for method, _, _ in self.calls))

    def test_cleanup_failure_fails_the_job(self):
        def fail_delete(method, path, body=None):
            return (500, {}) if method == "DELETE" and path.startswith("/sessions/") else self.api(method, path, body)

        with self.assertRaisesRegex(SystemExit, "remove replaced session"):
            self.run_seed(fail_delete)

    def test_partial_seed_retries_missing_tasks(self):
        self.run_seed()
        session_id = seed.stable("website/session/annotator")
        self.assertGreater(len(self.sessions[session_id]), 1)
        self.sessions[session_id].pop()
        self.sessions[session_id].append({"id": "user-continuation"})
        self.run_seed()
        self.assertEqual(len(self.sessions[session_id]), len(self.fixtures["annotator"]["tasks"]) + 1)

    def test_task_list_error_does_not_replay_or_delete(self):
        def fail_list(method, path, body=None):
            return (503, {}) if path.endswith("/tasks") else self.api(method, path, body)

        with self.assertRaisesRegex(SystemExit, "list tasks"):
            self.run_seed(fail_list)
        self.assertFalse(any(method == "DELETE" or path == "/tasks" for method, path, _ in self.calls))

    def test_task_cleanup_failure_keeps_session_for_retry(self):
        def fail_delete(method, path, body=None):
            return (500, {}) if method == "DELETE" and path.startswith("/tasks/") else self.api(method, path, body)

        with self.assertRaisesRegex(SystemExit, "remove replaced task"):
            self.run_seed(fail_delete)
        self.assertTrue(all(seed.stable(f"session/{key}") in self.sessions for key in seed.REPLACED_KEYS))

    def test_unexpected_fixture_is_refused_before_contacting_controller(self):
        with patch.object(seed.json, "load", return_value={**self.fixtures, "extra": {}}):
            with self.assertRaisesRegex(SystemExit, "exactly the five"):
                self.run_seed()
        self.assertEqual(self.calls, [])


if __name__ == "__main__":
    unittest.main()
