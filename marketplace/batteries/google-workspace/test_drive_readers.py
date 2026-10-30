import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import unittest
import urllib.error


SCRIPT = Path(__file__).with_name("drive-readers.py")
SPEC = importlib.util.spec_from_file_location("drive_readers", SCRIPT)
ANNOTATOR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ANNOTATOR)
SOURCE = ANNOTATOR.SOURCE

USERINFO = SOURCE.USERINFO_URL
DIRECTORY = SOURCE.DIRECTORY_ROOT
DRIVE = ANNOTATOR.DRIVE_ROOT
LIST = {
    "supportsAllDrives": "true",
    "includePermissionsForView": "published",
    "pageSize": 100,
    "fields": "nextPageToken,permissions(type,emailAddress,domain,deleted)",
}
VIEWER = {"email": "alice@corp.com", "email_verified": True, "hd": "corp.com"}
FORBIDDEN = SOURCE.Forbidden("403")


def permissions(file_id, *grants):
    return (f"{DRIVE}/files/{file_id}/permissions", LIST, {"permissions": list(grants)})


def user(email, **extra):
    return {"type": "user", "emailAddress": email, **extra}


def drive(*responses):
    """A Drive answering from recorded API payloads; an unexpected call fails."""
    remaining = [(USERINFO, {}, VIEWER), *responses]

    def call(url, **params):
        for index, (fixture_url, fixture_params, response) in enumerate(remaining):
            if fixture_url == url and fixture_params == params:
                remaining.pop(index)
                if isinstance(response, Exception):
                    raise response
                return response
        raise AssertionError(f"unexpected call {url} {params}")

    return ANNOTATOR.Drive(call)


def required(answer):
    return answer["requires"]["audience"]["contains"]


class TargetTests(unittest.TestCase):
    def test_a_file_created_in_a_folder_anyone_with_the_link_reads_needs_public_data(self):
        # Issue #747: the folder's sharing reaches the new file.
        answer = ANNOTATOR.answer(
            drive(permissions("F", user("alice@corp.com"), {"type": "anyone"})),
            "create_file",
            {"title": "diff", "textContent": "...", "parentId": "F"},
        )
        self.assertEqual(required(answer), "public")
        self.assertEqual(answer["requires"]["trust"], "trusted")
        self.assertEqual(answer["emits"], ["google-workspace.changed"])
        self.assertNotIn("forwards", answer)

    def test_a_folder_the_workspace_domain_reads_needs_internal_data(self):
        answer = ANNOTATOR.answer(
            drive(permissions("F", user("alice@corp.com"), {"type": "domain", "domain": "corp.com"})),
            "create_file",
            {"parentId": "F"},
        )
        self.assertEqual(required(answer), ["internal"])

    def test_another_domain_or_an_outside_account_needs_public_data(self):
        for grant, lookups in [
            ({"type": "domain", "domain": "partner.com"}, []),
            (user("bob@partner.com"), [(f"{DIRECTORY}/users/bob%40partner.com", {}, SOURCE.NotFound("404"))]),
            (user("ex@corp.com"), [(f"{DIRECTORY}/users/ex%40corp.com", {}, {"suspended": True})]),
        ]:
            with self.subTest(grant=grant):
                answer = ANNOTATOR.answer(drive(permissions("F", grant), *lookups), "create_file", {"parentId": "F"})
                self.assertEqual(required(answer), "public")

    def test_a_folder_shared_with_an_active_colleague_needs_internal_data(self):
        answer = ANNOTATOR.answer(
            drive(
                permissions("F", user("alice@corp.com"), user("Carol@corp.com")),
                (f"{DIRECTORY}/users/carol%40corp.com", {}, {"primaryEmail": "carol@corp.com"}),
            ),
            "create_file",
            {"parentId": "F"},
        )
        self.assertEqual(required(answer), ["internal"])

    def test_a_folder_only_the_viewer_reads_needs_self_data(self):
        answer = ANNOTATOR.answer(drive(permissions("F", user("alice@corp.com"))), "create_file", {"parentId": "F"})
        self.assertEqual(required(answer), ["self"])
        # No parent is the viewer's My Drive root, which nobody else reads.
        self.assertEqual(required(ANNOTATOR.answer(drive(), "create_file", {"title": "x"})), ["self"])

    def test_a_deleted_grantee_reads_nothing(self):
        answer = ANNOTATOR.answer(
            drive(permissions("F", user("alice@corp.com"), {"type": "anyone", "deleted": True})),
            "create_file",
            {"parentId": "F"},
        )
        self.assertEqual(required(answer), ["self"])

    def test_a_group_is_internal_only_when_every_member_is(self):
        group = f"{DIRECTORY}/groups/eng%40corp.com/members"
        leads = f"{DIRECTORY}/groups/leads%40corp.com/members"
        internal = drive(
            permissions("F", {"type": "group", "emailAddress": "eng@corp.com"}),
            (group, {"maxResults": 200}, {"members": [
                {"type": "USER", "email": "alice@corp.com"},
                {"type": "GROUP", "email": "leads@corp.com"},
            ]}),
            (leads, {"maxResults": 200}, {"members": [{"type": "USER", "email": "alice@corp.com"}]}),
        )
        self.assertEqual(required(ANNOTATOR.answer(internal, "create_file", {"parentId": "F"})), ["internal"])

        outside = drive(
            permissions("F", {"type": "group", "emailAddress": "eng@corp.com"}),
            (group, {"maxResults": 200}, {"members": [{"type": "USER", "email": "bob@partner.com"}]}),
            (f"{DIRECTORY}/users/bob%40partner.com", {}, SOURCE.NotFound("404")),
        )
        self.assertEqual(required(ANNOTATOR.answer(outside, "create_file", {"parentId": "F"})), "public")

        hidden = drive(
            permissions("F", {"type": "group", "emailAddress": "eng@corp.com"}),
            (group, {"maxResults": 200}, FORBIDDEN),
        )
        self.assertEqual(required(ANNOTATOR.answer(hidden, "create_file", {"parentId": "F"})), "public")

    def test_readers_the_token_may_not_list_could_be_anyone(self):
        answer = ANNOTATOR.answer(
            drive((f"{DRIVE}/files/F/permissions", LIST, FORBIDDEN)), "create_file", {"parentId": "F"}
        )
        self.assertEqual(required(answer), "public")

    def test_any_other_api_error_refuses_the_call(self):
        with self.assertRaises(RuntimeError):
            ANNOTATOR.answer(
                drive((f"{DRIVE}/files/F/permissions", LIST, RuntimeError("500"))), "create_file", {"parentId": "F"}
            )


class SourceTests(unittest.TestCase):
    def test_a_copy_forwards_who_reads_the_source(self):
        # A source the viewer alone reads, copied into a folder the
        # Workspace reads: the copy needs `internal`, the content is `self`,
        # so the engine refuses it.
        answer = ANNOTATOR.answer(
            drive(
                permissions("S", user("alice@corp.com")),
                permissions("F", {"type": "domain", "domain": "corp.com"}),
            ),
            "copy_file",
            {"fileId": "S", "parentId": "F"},
        )
        self.assertEqual(answer["forwards"], {"audience": ["self"]})
        self.assertEqual(required(answer), ["internal"])

    def test_a_source_shared_with_one_outsider_forwards_the_viewer_not_everyone(self):
        answer = ANNOTATOR.answer(
            drive(
                permissions("S", user("alice@corp.com"), user("bob@partner.com")),
                permissions("F", user("alice@corp.com")),
            ),
            "copy_file",
            {"fileId": "S", "parentId": "F"},
        )
        self.assertEqual(answer["forwards"], {"audience": ["self"]})

    def test_a_domain_or_public_source_forwards_that_audience(self):
        for grant, forwarded in [({"type": "domain", "domain": "corp.com"}, ["internal"]), ({"type": "anyone"}, "public")]:
            with self.subTest(grant=grant):
                answer = ANNOTATOR.answer(
                    drive(permissions("S", grant), permissions("F", user("alice@corp.com"))),
                    "copy_file",
                    {"fileId": "S", "parentId": "F"},
                )
                self.assertEqual(answer["forwards"], {"audience": forwarded})

    def test_a_source_whose_readers_are_hidden_stays_with_the_viewer(self):
        answer = ANNOTATOR.answer(
            drive((f"{DRIVE}/files/S/permissions", LIST, FORBIDDEN), permissions("F", user("alice@corp.com"))),
            "copy_file",
            {"fileId": "S", "parentId": "F"},
        )
        self.assertEqual(answer["forwards"], {"audience": ["self"]})

    def test_a_copy_without_a_parent_lands_beside_the_original(self):
        answer = ANNOTATOR.answer(
            drive(
                (f"{DRIVE}/files/S", {"supportsAllDrives": "true", "fields": "parents"}, {"parents": ["P"]}),
                permissions("S", {"type": "anyone"}),
                permissions("P", {"type": "anyone"}),
            ),
            "copy_file",
            {"fileId": "S"},
        )
        self.assertEqual(required(answer), "public")

    def test_a_rename_reaches_the_file_readers_and_forwards_nothing(self):
        answer = ANNOTATOR.answer(
            drive(permissions("S", {"type": "domain", "domain": "corp.com"})),
            "update_file",
            {"fileId": "S", "title": "Q3 layoffs"},
        )
        self.assertEqual(required(answer), ["internal"])
        self.assertNotIn("forwards", answer)

    def test_a_move_adds_the_folder_readers_and_forwards_the_file(self):
        answer = ANNOTATOR.answer(
            drive(permissions("S", user("alice@corp.com")), permissions("F", {"type": "anyone"})),
            "update_file",
            {"fileId": "S", "parentId": "F"},
        )
        self.assertEqual(required(answer), "public")
        self.assertEqual(answer["forwards"], {"audience": ["self"]})


PARENTS = {"supportsAllDrives": "true", "fields": "parents"}


class UpdateTests(unittest.TestCase):
    def test_re_enabling_inheritance_adds_the_parent_readers_and_forwards_the_folder(self):
        for grant, expected in [({"type": "anyone"}, "public"), ({"type": "domain", "domain": "corp.com"}, ["internal"])]:
            with self.subTest(grant=grant):
                answer = ANNOTATOR.answer(
                    drive(
                        permissions("D", user("alice@corp.com")),
                        (f"{DRIVE}/files/D", PARENTS, {"parents": ["P"]}),
                        permissions("P", grant),
                    ),
                    "update_file",
                    {"fileId": "D", "inheritedPermissionsDisabled": False},
                )
                self.assertEqual(required(answer), expected)
                self.assertEqual(answer["forwards"], {"audience": ["self"]})

    def test_re_enabling_inheritance_under_a_parent_the_token_cannot_see_needs_public_data(self):
        # Unlike a copy, whose hidden parent sends it to the viewer's root.
        answer = ANNOTATOR.answer(
            drive(permissions("D", user("alice@corp.com")), (f"{DRIVE}/files/D", PARENTS, {})),
            "update_file",
            {"fileId": "D", "inheritedPermissionsDisabled": False},
        )
        self.assertEqual(required(answer), "public")

    def test_disabling_inheritance_only_narrows_and_asks_for_no_parent(self):
        answer = ANNOTATOR.answer(
            drive(permissions("D", user("alice@corp.com"))),
            "update_file",
            {"fileId": "D", "inheritedPermissionsDisabled": True},
        )
        self.assertEqual(required(answer), ["self"])
        self.assertNotIn("forwards", answer)

    def test_letting_writers_share_needs_public_data(self):
        for value, expected in [(True, "public"), (False, ["self"])]:
            with self.subTest(writersCanShare=value):
                answer = ANNOTATOR.answer(
                    drive(permissions("D", user("alice@corp.com"))),
                    "update_file",
                    {"fileId": "D", "writersCanShare": value},
                )
                self.assertEqual(required(answer), expected)

    def test_an_unknown_argument_or_a_non_boolean_flag_refuses_the_call(self):
        for arguments in [
            {"fileId": "D", "copyRequiresWriterPermission": False},
            {"fileId": "D", "inheritedPermissionsDisabled": "false"},
        ]:
            with self.subTest(arguments=arguments), self.assertRaises(ValueError):
                ANNOTATOR.answer(drive(permissions("D", user("alice@corp.com"))), "update_file", arguments)


class ConsultTests(unittest.TestCase):
    def run_script(self, audiences, env):
        consult = {
            "version": 1,
            "kind": "annotation",
            "name": ANNOTATOR.NAME,
            "declaration": {"audiences": audiences},
            "artifact": {"args": {"name": "mcp/claude_ai_Google_Drive/create_file", "arguments": {}}, "context": {}},
        }
        return subprocess.run(
            [sys.executable, "-I", str(SCRIPT)],
            input=json.dumps(consult),
            capture_output=True,
            text=True,
            env=env,
        )

    def test_a_foreign_mandate_is_refused_before_the_token_is_read(self):
        result = self.run_script(["self"], {"APPA_PROVIDER_GOOGLE_WORKSPACE_TOKEN": "ya29.x"})
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertEqual(result.stdout, "")

    def test_a_missing_token_answers_nothing(self):
        result = self.run_script(["self", "internal"], {})
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
