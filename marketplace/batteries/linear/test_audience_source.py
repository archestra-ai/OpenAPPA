"""Offline tests of the actual helper, including its process envelope."""

import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import unittest

SPEC = importlib.util.spec_from_file_location("linear_audience", Path(__file__).with_name("audience-source.py"))
source = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(source)
SCOPE = "00000000-0000-0000-0000-000000000001"
ALICE = "00000000-0000-0000-0000-000000000002"
BOB = "00000000-0000-0000-0000-000000000003"
GUEST = "00000000-0000-0000-0000-000000000004"


def user(id=ALICE, active=True, guest=False):
    return {"id": id, "active": active, "guest": guest}


def page(users, next=False, cursor=None, team=False, scope=SCOPE):
    return {"team" if team else "organization": {
        "id": scope, "members" if team else "users": {
            "nodes": users, "pageInfo": {"hasNextPage": next, "endCursor": cursor}}}}


class SourceTests(unittest.TestCase):
    def test_viewer_uses_qualified_id_not_profile_email(self):
        data = user() | {"email": "unverified@example.com"}
        self.assertEqual(source.answer(lambda *_: {"viewer": data}, {"selector": "viewer"}),
                         {"members": ["linear:" + ALICE]})

    def test_workspace_pages_exclude_guests_and_inactive_users(self):
        calls = []
        def call(query, variables):
            calls.append(variables)
            return (page([user(), user(GUEST, guest=True)], True, "next") if variables["after"] is None
                    else page([user(BOB, active=False)]))
        self.assertEqual(source.answer(call, {"selector": f"workspace/{SCOPE}/members"}),
                         {"members": ["linear:" + ALICE]})
        self.assertEqual(calls, [{"after": None}, {"after": "next"}])

    def test_named_team_includes_its_active_guests(self):
        def call(query, variables):
            self.assertEqual(variables, {"id": SCOPE, "after": None})
            return page([user(GUEST, guest=True)], team=True)
        self.assertEqual(source.answer(call, {"selector": f"team/{SCOPE}/members"}),
                         {"members": ["linear:" + GUEST]})

    def test_wrong_workspace_is_not_internal(self):
        with self.assertRaises(source.Refusal):
            source.answer(lambda *_: page([user()], scope=BOB), {"selector": f"workspace/{SCOPE}/members"})

    def test_lookup_null_preserves_qualified_reader_without_network(self):
        self.assertEqual(source.answer(None, {"member": "linear:" + ALICE}), {"principal": None})

    def test_bad_inputs_refuse(self):
        for artifact in [{"member": "github:alice"}, {"member": "linear:public"},
                         {"selector": "full-members"}, {"selector": f"team/{SCOPE}/members", "member": ALICE},
                         {"selector": "team/x) { users { id } }/members"}, None, {"selector": 4}]:
            with self.subTest(artifact=artifact), self.assertRaises(source.Refusal):
                source.answer(None, artifact)

    def test_malformed_or_duplicate_member_invalidates_whole_answer(self):
        for users in [[user(), user()], [user(), {"id": BOB}], [user("public")],
                      [user(active=1)], [user(guest=None)]]:
            with self.subTest(users=users), self.assertRaises(source.Refusal):
                source.members(lambda *_: page(users), "workspace", SCOPE)

    def test_missing_or_repeated_cursor_refuses(self):
        for cursor in [None, "", "repeated"]:
            with self.subTest(cursor=cursor), self.assertRaises(source.Refusal):
                source.members(lambda *_: page([], True, cursor), "workspace", SCOPE)

    def test_later_page_failure_does_not_return_partial_membership(self):
        def call(query, variables):
            if variables["after"] is not None:
                raise source.Refusal("offline")
            return page([user()], True, "next")
        with self.assertRaises(source.Refusal):
            source.members(call, "workspace", SCOPE)

    def test_inactive_viewer_refuses(self):
        with self.assertRaises(source.Refusal):
            source.answer(lambda *_: {"viewer": user(active=False)}, {"selector": "viewer"})

    def test_http_errors_are_not_partial_success_or_secret_logs(self):
        class Client:
            def open(self, request, timeout):
                raise OSError("secret-token")
        with self.assertRaisesRegex(source.Refusal, "^Linear API request failed$"):
            source.graphql("lin_api_secret", Client())(source.VIEWER, {})

    def test_graphql_errors_refuse_even_with_data(self):
        class Client:
            def open(self, request, timeout):
                return io.BytesIO(json.dumps({"data": {"viewer": user()}, "errors": [{"message": "partial"}]}).encode())
        with self.assertRaises(source.Refusal):
            source.graphql("lin_api_secret", Client())(source.VIEWER, {})

    def test_redirect_does_not_forward_credentials(self):
        with self.assertRaises(source.Refusal):
            source.NoRedirect().redirect_request(None, None, 302, "", {}, "https://elsewhere.example")

    def test_helper_process_and_invalid_envelopes(self):
        request = {"version": 1, "kind": "audience", "name": "linear", "artifact": {"member": "linear:" + ALICE}}
        env = dict(os.environ)
        env.pop(source.TOKEN_ENV, None)
        def run(payload):
            return subprocess.run([sys.executable, str(Path(__file__).with_name("audience-source.py"))],
                                  input=json.dumps(payload), text=True, capture_output=True, env=env, timeout=5)
        result = run(request)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), {"version": 1, "answer": {"principal": None}})
        for invalid in [request | {"version": True}, request | {"name": "github"},
                        request | {"artifact": {"selector": "viewer"}}]:
            result = run(invalid)
            self.assertEqual(result.returncode, 1)
            self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
