import importlib.util
import json
import os
import stat
import subprocess
import sys
import tempfile
import threading
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("audience-source.py")
SPEC = importlib.util.spec_from_file_location("audience_source", SCRIPT)
AUDIENCE_SOURCE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(AUDIENCE_SOURCE)

USER_ATTRIBUTES = ("--attributes", "id,userName,active")
GROUP_ATTRIBUTES = ("--attributes", "id,displayName,members")


def fixture_cli(responses):
    """A CLI runner answering recorded `databricks` outputs, each once, by
    the exact arguments the source passes."""

    remaining = list(responses)
    # The source looks members up from several threads at once.
    taking = threading.Lock()

    def run(*args):
        with taking:
            for index, (fixture_args, response) in enumerate(remaining):
                if fixture_args == args:
                    remaining.pop(index)
                    break
            else:
                raise AssertionError(f"unexpected command databricks {' '.join(args)}")
        if isinstance(response, Exception):
            raise response
        return response

    return run


def user(id, user_name, active=True):
    # The CLI leaves a false `active` out, as the SDK renders it.
    return {"id": id, "userName": user_name, "active": True} if active else {"id": id, "userName": user_name}


def group(id, name, users=(), groups=()):
    members = [{"value": user_id, "$ref": f"Users/{user_id}", "type": "User"} for user_id in users]
    members += [{"value": group_id, "$ref": f"Groups/{group_id}", "type": "Group"} for group_id in groups]
    return {"id": id, "displayName": name, "members": members}


def users_listing(*filter_args):
    return ("users", "list", *USER_ATTRIBUTES, *filter_args)


def group_listing(name):
    return ("groups", "list", "--filter", f'displayName eq "{name}"', *GROUP_ATTRIBUTES)


def user_lookup(user_id, entry):
    return (("users", "get", user_id), entry)


def space_permissions(space_id, acl):
    return (("permissions", "get", "genie", space_id), acl)


def not_found():
    return AUDIENCE_SOURCE.NotFound("Error: User with id 9 not found.")


class SelectorTests(unittest.TestCase):
    def test_viewer_is_the_logins_own_address(self):
        run = fixture_cli([(("current-user", "me"), user("1", "alice@corp.com"))])
        self.assertEqual(AUDIENCE_SOURCE.answer(run, {"selector": "viewer"}), {"members": ["alice@corp.com"]})

    def test_a_viewer_without_an_address_is_the_qualified_id(self):
        run = fixture_cli([(("current-user", "me"), user("1", "svc-agent"))])
        self.assertEqual(AUDIENCE_SOURCE.answer(run, {"selector": "viewer"}), {"members": ["databricks:1"]})

    def test_an_inactive_viewer_reads_nothing(self):
        run = fixture_cli([(("current-user", "me"), user("1", "alice@corp.com", active=False))])
        self.assertEqual(AUDIENCE_SOURCE.answer(run, {"selector": "viewer"}), {"members": []})

    def test_members_are_the_active_users_once_each(self):
        listed = [user(str(i), f"u{i}@corp.com") for i in range(100)]
        listed += [user("100", "bob@corp.com"), user("101", "bob@corp.com"), user("102", "svc-agent")]
        run = fixture_cli([(users_listing("--filter", "active eq true"), listed)])
        members = AUDIENCE_SOURCE.answer(run, {"selector": "members"})["members"]
        self.assertEqual(members, [f"u{i}@corp.com" for i in range(100)] + ["bob@corp.com", "databricks:102"])

    def test_a_directory_over_the_bound_is_refused(self):
        listed = [user(str(i), f"u{i}@corp.com") for i in range(5001)]
        run = fixture_cli([(users_listing("--filter", "active eq true"), listed)])
        with self.assertRaises(RuntimeError):
            AUDIENCE_SOURCE.answer(run, {"selector": "members"})

    def test_a_listing_that_is_not_a_list_is_a_failure(self):
        run = fixture_cli([(users_listing("--filter", "active eq true"), {"Resources": [user("1", "a@corp.com")]})])
        with self.assertRaises(RuntimeError):
            AUDIENCE_SOURCE.answer(run, {"selector": "members"})

    def test_a_group_expands_its_users_and_nested_groups_leaving_inactive_ones_out(self):
        run = fixture_cli(
            [
                (group_listing("finance"), [group("g1", "finance", users=["1", "2"], groups=["g2"])]),
                user_lookup("1", user("1", "alice@corp.com")),
                user_lookup("2", user("2", "gone@corp.com", active=False)),
                (("groups", "get", "g2"), group("g2", "controllers", users=["3", "1"])),
                user_lookup("3", user("3", "carol@corp.com")),
            ]
        )
        members = AUDIENCE_SOURCE.answer(run, {"selector": "group/finance"})["members"]
        self.assertEqual(members, ["alice@corp.com", "carol@corp.com"])

    def test_a_nested_group_is_recognised_by_its_ref_alone(self):
        finance = {"id": "g1", "displayName": "finance", "members": [{"value": "g2", "$ref": "Groups/g2"}]}
        run = fixture_cli(
            [
                (group_listing("finance"), [finance]),
                (("groups", "get", "g2"), group("g2", "controllers", users=["3"])),
                user_lookup("3", user("3", "carol@corp.com")),
            ]
        )
        members = AUDIENCE_SOURCE.answer(run, {"selector": "group/finance"})["members"]
        self.assertEqual(members, ["carol@corp.com"])

    def test_nested_groups_are_read_in_one_directory_listing_past_the_direct_bound(self):
        nested = [f"g{i}" for i in range(1, 6)]
        run = fixture_cli(
            [
                (group_listing("all"), [group("g0", "all", groups=nested)]),
                *[(("groups", "get", g), group(g, g, users=[f"{g}-{i}" for i in range(5)])) for g in nested],
                (users_listing(), [user(f"{g}-{i}", f"{g}-{i}@corp.com") for g in nested for i in range(5)]),
            ]
        )
        members = AUDIENCE_SOURCE.answer(run, {"selector": "group/all"})["members"]
        self.assertEqual(members, [f"{g}-{i}@corp.com" for g in nested for i in range(5)])

    def test_a_name_with_quotes_is_escaped_in_the_filter(self):
        run = fixture_cli(
            [
                (("groups", "list", "--filter", 'displayName eq "a\\"b\\\\c"', *GROUP_ATTRIBUTES), [group("g1", 'a"b\\c', users=["1"])]),
                user_lookup("1", user("1", "alice@corp.com")),
            ]
        )
        members = AUDIENCE_SOURCE.answer(run, {"selector": 'group/a"b\\c'})["members"]
        self.assertEqual(members, ["alice@corp.com"])

    def test_a_large_group_reads_the_directory_once(self):
        ids = [str(i) for i in range(21)]
        run = fixture_cli(
            [
                (group_listing("everyone"), [group("g1", "everyone", users=ids)]),
                (users_listing(), [user(i, f"u{i}@corp.com") for i in ids]),
            ]
        )
        members = AUDIENCE_SOURCE.answer(run, {"selector": "group/everyone"})["members"]
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
        run = fixture_cli(
            [
                space_permissions("space-1", acl),
                (group_listing("east"), [group("g1", "east", users=first)]),
                (group_listing("west"), [group("g2", "west", users=second)]),
                (users_listing(), [user(str(i), f"u{i}@corp.com") for i in range(31)]),
            ]
        )
        members = AUDIENCE_SOURCE.answer(run, {"selector": "genie-space/space-1/readers"})["members"]
        self.assertEqual(members, [f"u{i}@corp.com" for i in range(31)])

    def test_a_group_member_the_directory_does_not_report_is_a_failure(self):
        run = fixture_cli(
            [
                (group_listing("finance"), [group("g1", "finance", users=["1"])]),
                user_lookup("1", not_found()),
            ]
        )
        with self.assertRaises(RuntimeError):
            AUDIENCE_SOURCE.answer(run, {"selector": "group/finance"})

    def test_a_group_name_matching_no_group_or_several_is_a_failure(self):
        for listed in [[], [group("g1", "finance"), group("g2", "finance")]]:
            run = fixture_cli([(group_listing("finance"), listed)])
            with self.assertRaises(RuntimeError):
                AUDIENCE_SOURCE.answer(run, {"selector": "group/finance"})

    def test_a_shared_or_cyclic_subgroup_is_read_and_expanded_once(self):
        run = fixture_cli(
            [
                (group_listing("loop"), [group("g1", "loop", users=["1"], groups=["g2", "g3"])]),
                (("groups", "get", "g2"), group("g2", "left", users=["2"], groups=["g4", "g1"])),
                (("groups", "get", "g3"), group("g3", "right", groups=["g4"])),
                (("groups", "get", "g4"), group("g4", "shared", users=["3"])),
                user_lookup("1", user("1", "alice@corp.com")),
                user_lookup("2", user("2", "bob@corp.com")),
                user_lookup("3", user("3", "carol@corp.com")),
            ]
        )
        members = AUDIENCE_SOURCE.answer(run, {"selector": "group/loop"})["members"]
        self.assertEqual(members, ["alice@corp.com", "bob@corp.com", "carol@corp.com"])

    def test_a_space_whose_groups_share_a_subgroup_reads_it_once(self):
        acl = {
            "access_control_list": [
                {"group_name": "east", "all_permissions": [{"permission_level": "CAN_VIEW"}]},
                {"group_name": "west", "all_permissions": [{"permission_level": "CAN_VIEW"}]},
            ]
        }
        run = fixture_cli(
            [
                space_permissions("space-1", acl),
                (group_listing("east"), [group("g1", "east", groups=["g3"])]),
                (group_listing("west"), [group("g2", "west", groups=["g3"])]),
                (("groups", "get", "g3"), group("g3", "shared", users=["1"])),
                user_lookup("1", user("1", "alice@corp.com")),
            ]
        )
        members = AUDIENCE_SOURCE.answer(run, {"selector": "genie-space/space-1/readers"})["members"]
        self.assertEqual(members, ["alice@corp.com"])

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
        run = fixture_cli(
            [
                space_permissions("space-1", acl),
                (users_listing("--filter", 'userName eq "alice@corp.com"'), [user("1", "alice@corp.com")]),
                (group_listing("analysts"), [group("g1", "analysts", users=["1", "2"])]),
                user_lookup("1", user("1", "alice@corp.com")),
                user_lookup("2", user("2", "bob@corp.com")),
            ]
        )
        members = AUDIENCE_SOURCE.answer(run, {"selector": "genie-space/space-1/readers"})["members"]
        self.assertEqual(members, ["databricks:app-uuid", "alice@corp.com", "bob@corp.com"])

    def test_a_space_shared_with_many_users_reads_the_directory_once(self):
        acl = {
            "access_control_list": [
                {"user_name": f"u{i}@corp.com", "all_permissions": [{"permission_level": "CAN_VIEW"}]} for i in range(21)
            ]
        }
        run = fixture_cli(
            [
                space_permissions("space-1", acl),
                (users_listing(), [user(str(i), f"u{i}@corp.com") for i in range(30)]),
            ]
        )
        members = AUDIENCE_SOURCE.answer(run, {"selector": "genie-space/space-1/readers"})["members"]
        self.assertEqual(members, [f"u{i}@corp.com" for i in range(21)])

    def test_a_space_user_the_directory_does_not_report_is_a_failure(self):
        acl = {
            "access_control_list": [
                {"user_name": f"u{i}@corp.com", "all_permissions": [{"permission_level": "CAN_VIEW"}]} for i in range(21)
            ]
        }
        run = fixture_cli(
            [
                space_permissions("space-1", acl),
                (users_listing(), [user(str(i), f"u{i}@corp.com") for i in range(20)]),
            ]
        )
        with self.assertRaises(RuntimeError):
            AUDIENCE_SOURCE.answer(run, {"selector": "genie-space/space-1/readers"})

    def test_a_permission_entry_without_a_principal_is_a_failure(self):
        acl = {"access_control_list": [{"all_permissions": [{"permission_level": "CAN_VIEW"}]}]}
        run = fixture_cli([space_permissions("space-1", acl)])
        with self.assertRaises(RuntimeError):
            AUDIENCE_SOURCE.answer(run, {"selector": "genie-space/space-1/readers"})

    def test_a_space_the_workspace_does_not_report_is_a_failure(self):
        run = fixture_cli([space_permissions("space-1", not_found())])
        with self.assertRaises(AUDIENCE_SOURCE.NotFound):
            AUDIENCE_SOURCE.answer(run, {"selector": "genie-space/space-1/readers"})

    def test_an_unserved_selector_is_refused_before_any_command(self):
        run = fixture_cli([])
        for selector in ["", "group/", "genie-space//readers", "genie-space/a/b/readers", "genie-space/space-1", "channel/C1", 3]:
            with self.assertRaises(ValueError):
                AUDIENCE_SOURCE.answer(run, {"selector": selector})


class MemberTests(unittest.TestCase):
    def test_a_member_resolves_to_its_address(self):
        run = fixture_cli([user_lookup("1", user("1", "alice@corp.com"))])
        self.assertEqual(AUDIENCE_SOURCE.answer(run, {"member": "databricks:1"}), {"principal": "alice@corp.com"})

    def test_a_member_without_an_address_stays_as_written(self):
        run = fixture_cli([user_lookup("1", user("1", "svc-agent"))])
        self.assertEqual(AUDIENCE_SOURCE.answer(run, {"member": "databricks:1"}), {"principal": "databricks:1"})

    def test_an_inactive_member_stays_as_written(self):
        run = fixture_cli([user_lookup("1", user("1", "alice@corp.com", active=False))])
        self.assertEqual(AUDIENCE_SOURCE.answer(run, {"member": "databricks:1"}), {"principal": "databricks:1"})

    def test_an_unknown_member_is_null(self):
        run = fixture_cli([user_lookup("9", not_found())])
        self.assertEqual(AUDIENCE_SOURCE.answer(run, {"member": "databricks:9"}), {"principal": None})

    def test_a_foreign_member_is_refused(self):
        run = fixture_cli([])
        for member in ["slack:U1", "alice@corp.com", "databricks:"]:
            with self.assertRaises(ValueError):
                AUDIENCE_SOURCE.answer(run, {"member": member})

    def test_an_artifact_that_is_not_one_selector_or_member_is_refused(self):
        run = fixture_cli([])
        for artifact in [{}, {"selector": "viewer", "member": "databricks:1"}, {"other": 1}, "viewer", None]:
            with self.assertRaises(ValueError):
                AUDIENCE_SOURCE.answer(run, artifact)


class AddressTests(unittest.TestCase):
    def test_only_a_plain_address_is_a_reader(self):
        for text in ["alice@corp.com", "a.b+c@sub.corp.com"]:
            self.assertTrue(AUDIENCE_SOURCE.is_address(text))
        for text in ["svc-agent", "a@b@c", "@corp.com", "alice@", "alice @corp.com", "databricks:1@corp.com", "alice@corp.com:evil", None, 3]:
            self.assertFalse(AUDIENCE_SOURCE.is_address(text))


class FakeDatabricks:
    """A `databricks` executable on its own PATH entry: it records each
    invocation's arguments and environment, and answers with the script's
    stdout, stderr, and exit status."""

    def __init__(self, script):
        self.directory = tempfile.TemporaryDirectory()
        self.log = Path(self.directory.name) / "calls.jsonl"
        cli = Path(self.directory.name) / "databricks"
        cli.write_text(
            "#!/bin/sh\n"
            f"printf '%s\\n' \"$(python3 -c 'import json,os,sys; print(json.dumps({{\"args\": sys.argv[1:], \"token\": os.environ.get(\"DATABRICKS_TOKEN\")}}))' \"$@\")\" >> {self.log}\n"
            f"{script}\n"
        )
        cli.chmod(cli.stat().st_mode | stat.S_IEXEC)

    def environ(self, **extra):
        return {"PATH": f"{self.directory.name}:/usr/bin:/bin", **extra}

    def calls(self):
        if not self.log.exists():
            return []
        return [json.loads(line) for line in self.log.read_text().splitlines()]


class CliTests(unittest.TestCase):
    def run_cli(self, cli, *args, **extra):
        return AUDIENCE_SOURCE.databricks_cli(cli.environ(**extra))(*args)

    def test_a_command_is_run_with_json_output_and_its_answer_parsed(self):
        cli = FakeDatabricks("echo '{\"id\": \"1\", \"userName\": \"alice@corp.com\", \"active\": true}'")
        self.assertEqual(self.run_cli(cli, "current-user", "me"), user("1", "alice@corp.com"))
        self.assertEqual(cli.calls(), [{"args": ["current-user", "me", "-o", "json"], "token": None}])

    def test_the_bindings_token_reaches_the_cli_as_its_own_variable(self):
        cli = FakeDatabricks("echo '[]'")
        self.run_cli(cli, "users", "list", APPA_PROVIDER_DATABRICKS_TOKEN=" dapi-fixture\n")
        self.assertEqual(cli.calls()[0]["token"], "dapi-fixture")

    def test_a_missing_resource_is_not_found(self):
        cli = FakeDatabricks("echo 'Error: User with id 9 not found.' >&2; exit 1")
        with self.assertRaises(AUDIENCE_SOURCE.NotFound):
            self.run_cli(cli, "users", "get", "9")

    def test_any_other_failure_is_the_consults(self):
        for script in [
            "echo 'Error: default auth: cannot configure default credentials' >&2; exit 1",
            "echo 'Error: 404 Not Found' >&2; exit 1",
            "echo 'Error: RESOURCE_DOES_NOT_EXIST: no such space' >&2; exit 1",
            "echo 'not json'",
            "exit 3",
        ]:
            with self.assertRaises(RuntimeError):
                self.run_cli(FakeDatabricks(script), "users", "get", "9")

    def test_a_cli_missing_from_path_is_a_failure_naming_the_login(self):
        with self.assertRaises(RuntimeError) as refused:
            AUDIENCE_SOURCE.databricks_cli({"PATH": "/nonexistent"})("current-user", "me")
        self.assertIn("databricks auth login", str(refused.exception))


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

    def test_a_consult_is_answered_through_the_cli(self):
        cli = FakeDatabricks("echo '{\"id\": \"1\", \"userName\": \"alice@corp.com\", \"active\": true}'")
        result = self.run_script(self.envelope(), cli.environ())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), {"version": 1, "answer": {"members": ["alice@corp.com"]}})

    def test_a_foreign_envelope_is_refused(self):
        cli = FakeDatabricks("echo '{}'")
        for request in [self.envelope(version=2), self.envelope(kind="annotation"), self.envelope(name="slack")]:
            result = self.run_script(request, cli.environ())
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertEqual(result.stdout, "")
        self.assertEqual(cli.calls(), [])

    def test_a_failing_cli_is_a_failure_without_an_answer(self):
        cli = FakeDatabricks("echo 'Error: default auth: cannot configure default credentials' >&2; exit 1")
        result = self.run_script(self.envelope(), cli.environ())
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(result.stdout, "")

    def test_a_foreign_declaration_is_refused_before_the_cli_runs(self):
        cli = FakeDatabricks("echo '{}'")
        declared = self.envelope()["declaration"]["templates"]
        for templates in [declared + ["foreign/<x>"], declared[1:], [], list(reversed(declared))]:
            result = self.run_script(self.envelope(declaration={"templates": templates}), cli.environ())
            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertEqual(result.stdout, "")
        self.assertEqual(cli.calls(), [])


if __name__ == "__main__":
    unittest.main()
