import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import unittest


SCRIPT = Path(__file__).with_name("audience-source.py")
SPEC = importlib.util.spec_from_file_location("audience_source", SCRIPT)
AUDIENCE_SOURCE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(AUDIENCE_SOURCE)

SCIM = AUDIENCE_SOURCE.SCIM
USER_ATTRIBUTES = {"attributes": "id,userName,active"}
GROUP_ATTRIBUTES = {"attributes": "id,displayName,members"}


def fixture_api(responses):
    """A call answering from recorded Databricks REST payloads, in order."""

    remaining = list(responses)

    def call(path, **params):
        for index, (fixture_path, fixture_params, response) in enumerate(remaining):
            if fixture_path == path and fixture_params == params:
                remaining.pop(index)
                if isinstance(response, Exception):
                    raise response
                return response
        raise AssertionError(f"unexpected call {path} {params}")

    return call


def user(id, user_name, active=True):
    return {"id": id, "userName": user_name, "active": active}


def user_page(users, start=1, total=None):
    return {"totalResults": len(users) if total is None else total, "startIndex": start, "Resources": users}


def group(id, name, users=(), groups=()):
    members = [{"value": user_id, "$ref": f"Users/{user_id}", "type": "User"} for user_id in users]
    members += [{"value": group_id, "$ref": f"Groups/{group_id}", "type": "Group"} for group_id in groups]
    return {"id": id, "displayName": name, "members": members}


def users_listing(**params):
    return (f"{SCIM}/Users", {**USER_ATTRIBUTES, "count": 100, "startIndex": params.pop("startIndex", 1), **params})


def group_listing(name):
    return (f"{SCIM}/Groups", {"filter": f'displayName eq "{name}"', **GROUP_ATTRIBUTES})


def user_lookup(user_id, entry):
    return (f"{SCIM}/Users/{user_id}", {}, entry)


class SelectorTests(unittest.TestCase):
    def test_viewer_is_the_tokens_own_address(self):
        call = fixture_api([(f"{SCIM}/Me", {}, user("1", "alice@corp.com"))])
        self.assertEqual(AUDIENCE_SOURCE.answer(call, {"selector": "viewer"}), {"members": ["alice@corp.com"]})

    def test_a_viewer_without_an_address_is_the_qualified_id(self):
        call = fixture_api([(f"{SCIM}/Me", {}, user("1", "svc-agent"))])
        self.assertEqual(AUDIENCE_SOURCE.answer(call, {"selector": "viewer"}), {"members": ["databricks:1"]})

    def test_an_inactive_viewer_reads_nothing(self):
        call = fixture_api([(f"{SCIM}/Me", {}, user("1", "alice@corp.com", active=False))])
        self.assertEqual(AUDIENCE_SOURCE.answer(call, {"selector": "viewer"}), {"members": []})

    def test_members_pages_through_the_active_users(self):
        first = [user(str(i), f"u{i}@corp.com") for i in range(100)]
        second = [user("100", "bob@corp.com"), user("101", "bob@corp.com"), user("102", "svc-agent")]
        call = fixture_api(
            [
                (*users_listing(filter="active eq true"), user_page(first, total=103)),
                (*users_listing(filter="active eq true", startIndex=101), user_page(second, start=101, total=103)),
            ]
        )
        members = AUDIENCE_SOURCE.answer(call, {"selector": "members"})["members"]
        self.assertEqual(members, [f"u{i}@corp.com" for i in range(100)] + ["bob@corp.com", "databricks:102"])

    def test_a_directory_over_the_bound_is_refused_on_its_first_page(self):
        call = fixture_api([(*users_listing(filter="active eq true"), user_page([user("1", "a@corp.com")], total=5001))])
        with self.assertRaises(RuntimeError):
            AUDIENCE_SOURCE.answer(call, {"selector": "members"})

    def test_an_empty_page_before_the_total_is_a_failure_not_a_partial_answer(self):
        call = fixture_api([(*users_listing(filter="active eq true"), user_page([], total=3))])
        with self.assertRaises(RuntimeError):
            AUDIENCE_SOURCE.answer(call, {"selector": "members"})

    def test_a_listing_without_a_total_is_a_failure(self):
        call = fixture_api([(*users_listing(filter="active eq true"), {"Resources": [user("1", "a@corp.com")]})])
        with self.assertRaises(RuntimeError):
            AUDIENCE_SOURCE.answer(call, {"selector": "members"})

    def test_a_group_expands_its_users_and_nested_groups_leaving_inactive_ones_out(self):
        call = fixture_api(
            [
                (*group_listing("finance"), {"Resources": [group("g1", "finance", users=["1", "2"], groups=["g2"])]}),
                user_lookup("1", user("1", "alice@corp.com")),
                user_lookup("2", user("2", "gone@corp.com", active=False)),
                (f"{SCIM}/Groups/g2", GROUP_ATTRIBUTES, group("g2", "controllers", users=["3", "1"])),
                user_lookup("3", user("3", "carol@corp.com")),
            ]
        )
        members = AUDIENCE_SOURCE.answer(call, {"selector": "group/finance"})["members"]
        self.assertEqual(members, ["alice@corp.com", "carol@corp.com"])

    def test_nested_groups_are_read_in_one_directory_pass_past_the_direct_bound(self):
        nested = [f"g{i}" for i in range(1, 6)]
        call = fixture_api(
            [
                (*group_listing("all"), {"Resources": [group("g0", "all", groups=nested)]}),
                *[(f"{SCIM}/Groups/{g}", GROUP_ATTRIBUTES, group(g, g, users=[f"{g}-{i}" for i in range(5)])) for g in nested],
                (*users_listing(), user_page([user(f"{g}-{i}", f"{g}-{i}@corp.com") for g in nested for i in range(5)])),
            ]
        )
        members = AUDIENCE_SOURCE.answer(call, {"selector": "group/all"})["members"]
        self.assertEqual(members, [f"{g}-{i}@corp.com" for g in nested for i in range(5)])

    def test_a_name_with_quotes_is_escaped_in_the_filter(self):
        call = fixture_api(
            [
                (f"{SCIM}/Groups", {"filter": 'displayName eq "a\\"b\\\\c"', **GROUP_ATTRIBUTES}, {"Resources": [group("g1", 'a"b\\c', users=["1"])]}),
                user_lookup("1", user("1", "alice@corp.com")),
            ]
        )
        members = AUDIENCE_SOURCE.answer(call, {"selector": 'group/a"b\\c'})["members"]
        self.assertEqual(members, ["alice@corp.com"])

    def test_a_large_group_reads_the_directory_once(self):
        ids = [str(i) for i in range(21)]
        call = fixture_api(
            [
                (*group_listing("everyone"), {"Resources": [group("g1", "everyone", users=ids)]}),
                (*users_listing(), user_page([user(i, f"u{i}@corp.com") for i in ids])),
            ]
        )
        members = AUDIENCE_SOURCE.answer(call, {"selector": "group/everyone"})["members"]
        self.assertEqual(members, [f"u{i}@corp.com" for i in ids])

    def test_a_space_shared_with_several_large_groups_reads_the_directory_once(self):
        first = [str(i) for i in range(21)]
        second = [str(i) for i in range(10, 31)]
        acl = {
            "access_control_list": [
                {"group_name": "east", "all_permissions": [{"permission_level": "CAN_VIEW"}]},
                {"group_name": "west", "all_permissions": [{"permission_level": "CAN_VIEW"}]},
            ]
        }
        call = fixture_api(
            [
                ("/api/2.0/permissions/genie/space-1", {}, acl),
                (*group_listing("east"), {"Resources": [group("g1", "east", users=first)]}),
                (*group_listing("west"), {"Resources": [group("g2", "west", users=second)]}),
                (*users_listing(), user_page([user(str(i), f"u{i}@corp.com") for i in range(31)])),
            ]
        )
        members = AUDIENCE_SOURCE.answer(call, {"selector": "genie-space/space-1/readers"})["members"]
        self.assertEqual(members, [f"u{i}@corp.com" for i in range(31)])

    def test_a_group_member_the_directory_does_not_report_is_a_failure(self):
        call = fixture_api(
            [
                (*group_listing("finance"), {"Resources": [group("g1", "finance", users=["1"])]}),
                (f"{SCIM}/Users/1", {}, AUDIENCE_SOURCE.NotFound("/Users/1")),
            ]
        )
        with self.assertRaises(RuntimeError):
            AUDIENCE_SOURCE.answer(call, {"selector": "group/finance"})

    def test_a_group_name_matching_no_group_or_several_is_a_failure(self):
        for resources in [[], [group("g1", "finance"), group("g2", "finance")]]:
            call = fixture_api([(*group_listing("finance"), {"Resources": resources})])
            with self.assertRaises(RuntimeError):
                AUDIENCE_SOURCE.answer(call, {"selector": "group/finance"})

    def test_a_group_cycle_is_refused(self):
        listing = (*group_listing("loop"), {"Resources": [group("g1", "loop", groups=["g1"])]})
        nested = [(f"{SCIM}/Groups/g1", GROUP_ATTRIBUTES, group("g1", "loop", groups=["g1"])) for _ in range(10)]
        with self.assertRaises(RuntimeError):
            AUDIENCE_SOURCE.answer(fixture_api([listing, *nested]), {"selector": "group/loop"})

    def test_a_genie_space_collects_users_groups_and_service_principals(self):
        acl = {
            "object_id": "/genie/space-1",
            "access_control_list": [
                {"user_name": "alice@corp.com", "all_permissions": [{"permission_level": "CAN_MANAGE", "inherited": False}]},
                {"group_name": "analysts", "all_permissions": [{"permission_level": "CAN_VIEW", "inherited": True}]},
                {"service_principal_name": "app-uuid", "all_permissions": [{"permission_level": "CAN_RUN", "inherited": False}]},
                {"user_name": "nobody@corp.com", "all_permissions": []},
            ],
        }
        call = fixture_api(
            [
                ("/api/2.0/permissions/genie/space-1", {}, acl),
                (f"{SCIM}/Users", {"filter": 'userName eq "alice@corp.com"', **USER_ATTRIBUTES}, {"Resources": [user("1", "alice@corp.com")]}),
                (*group_listing("analysts"), {"Resources": [group("g1", "analysts", users=["1", "2"])]}),
                user_lookup("1", user("1", "alice@corp.com")),
                user_lookup("2", user("2", "bob@corp.com")),
            ]
        )
        members = AUDIENCE_SOURCE.answer(call, {"selector": "genie-space/space-1/readers"})["members"]
        self.assertEqual(members, ["databricks:app-uuid", "alice@corp.com", "bob@corp.com"])

    def test_a_space_shared_with_many_users_reads_the_directory_once(self):
        acl = {
            "access_control_list": [
                {"user_name": f"u{i}@corp.com", "all_permissions": [{"permission_level": "CAN_VIEW"}]} for i in range(21)
            ]
        }
        call = fixture_api(
            [
                ("/api/2.0/permissions/genie/space-1", {}, acl),
                (*users_listing(), user_page([user(str(i), f"u{i}@corp.com") for i in range(30)])),
            ]
        )
        members = AUDIENCE_SOURCE.answer(call, {"selector": "genie-space/space-1/readers"})["members"]
        self.assertEqual(members, [f"u{i}@corp.com" for i in range(21)])

    def test_a_space_user_the_directory_does_not_report_is_a_failure(self):
        acl = {
            "access_control_list": [
                {"user_name": f"u{i}@corp.com", "all_permissions": [{"permission_level": "CAN_VIEW"}]} for i in range(21)
            ]
        }
        call = fixture_api(
            [
                ("/api/2.0/permissions/genie/space-1", {}, acl),
                (*users_listing(), user_page([user(str(i), f"u{i}@corp.com") for i in range(20)])),
            ]
        )
        with self.assertRaises(RuntimeError):
            AUDIENCE_SOURCE.answer(call, {"selector": "genie-space/space-1/readers"})

    def test_a_permission_entry_without_a_principal_is_a_failure(self):
        acl = {"access_control_list": [{"all_permissions": [{"permission_level": "CAN_VIEW"}]}]}
        call = fixture_api([("/api/2.0/permissions/genie/space-1", {}, acl)])
        with self.assertRaises(RuntimeError):
            AUDIENCE_SOURCE.answer(call, {"selector": "genie-space/space-1/readers"})

    def test_a_space_the_workspace_does_not_report_is_a_failure(self):
        call = fixture_api([("/api/2.0/permissions/genie/space-1", {}, AUDIENCE_SOURCE.NotFound("/genie/space-1"))])
        with self.assertRaises(AUDIENCE_SOURCE.NotFound):
            AUDIENCE_SOURCE.answer(call, {"selector": "genie-space/space-1/readers"})

    def test_an_unserved_selector_is_refused_before_any_call(self):
        call = fixture_api([])
        for selector in ["", "group/", "genie-space//readers", "genie-space/a/b/readers", "genie-space/space-1", "channel/C1", 3]:
            with self.assertRaises(ValueError):
                AUDIENCE_SOURCE.answer(call, {"selector": selector})


class MemberTests(unittest.TestCase):
    def test_a_member_resolves_to_its_address(self):
        call = fixture_api([user_lookup("1", user("1", "alice@corp.com"))])
        self.assertEqual(AUDIENCE_SOURCE.answer(call, {"member": "databricks:1"}), {"principal": "alice@corp.com"})

    def test_a_member_without_an_address_stays_as_written(self):
        call = fixture_api([user_lookup("1", user("1", "svc-agent"))])
        self.assertEqual(AUDIENCE_SOURCE.answer(call, {"member": "databricks:1"}), {"principal": "databricks:1"})

    def test_an_inactive_member_stays_as_written(self):
        call = fixture_api([user_lookup("1", user("1", "alice@corp.com", active=False))])
        self.assertEqual(AUDIENCE_SOURCE.answer(call, {"member": "databricks:1"}), {"principal": "databricks:1"})

    def test_an_unknown_member_is_null(self):
        call = fixture_api([(f"{SCIM}/Users/9", {}, AUDIENCE_SOURCE.NotFound("/Users/9"))])
        self.assertEqual(AUDIENCE_SOURCE.answer(call, {"member": "databricks:9"}), {"principal": None})

    def test_a_foreign_member_is_refused(self):
        call = fixture_api([])
        for member in ["slack:U1", "alice@corp.com", "databricks:"]:
            with self.assertRaises(ValueError):
                AUDIENCE_SOURCE.answer(call, {"member": member})

    def test_an_artifact_that_is_not_one_selector_or_member_is_refused(self):
        call = fixture_api([])
        for artifact in [{}, {"selector": "viewer", "member": "databricks:1"}, {"other": 1}, "viewer", None]:
            with self.assertRaises(ValueError):
                AUDIENCE_SOURCE.answer(call, artifact)


class AddressTests(unittest.TestCase):
    def test_only_a_plain_address_is_a_reader(self):
        for text in ["alice@corp.com", "a.b+c@sub.corp.com"]:
            self.assertTrue(AUDIENCE_SOURCE.is_address(text))
        for text in ["svc-agent", "a@b@c", "@corp.com", "alice@", "alice @corp.com", "databricks:1@corp.com", None, 3]:
            self.assertFalse(AUDIENCE_SOURCE.is_address(text))


class EnvelopeTests(unittest.TestCase):
    def run_script(self, request, env):
        return subprocess.run(
            [sys.executable, str(SCRIPT)],
            input=json.dumps(request),
            capture_output=True,
            text=True,
            env=env,
            check=False,
        )

    def envelope(self, **overrides):
        return {
            "version": 1,
            "kind": "audience",
            "name": "databricks",
            "declaration": {"templates": ["viewer", "members", "group/<name>", "genie-space/<id>/readers"]},
            "artifact": {"selector": "viewer"},
            **overrides,
        }

    def test_a_foreign_envelope_is_refused(self):
        env = {"PATH": "/usr/bin:/bin", "APPA_PROVIDER_DATABRICKS_TOKEN": "dapi-fixture"}
        for request in [self.envelope(version=2), self.envelope(kind="annotation"), self.envelope(name="slack")]:
            result = self.run_script(request, env)
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertEqual(result.stdout, "")

    def test_a_missing_workspace_is_a_failure_before_any_network(self):
        result = self.run_script(self.envelope(), {"PATH": "/usr/bin:/bin"})
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("APPA_PROVIDER_DATABRICKS_HOST", result.stderr)

    def test_a_foreign_declaration_is_refused_before_the_token_is_read(self):
        declared = self.envelope()["declaration"]["templates"]
        for templates in [declared + ["foreign/<x>"], declared[1:], [], list(reversed(declared))]:
            result = self.run_script(self.envelope(declaration={"templates": templates}), {"PATH": "/usr/bin:/bin"})
            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
