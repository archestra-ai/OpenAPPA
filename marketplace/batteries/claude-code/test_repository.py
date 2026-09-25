import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("repository.py")
SPEC = importlib.util.spec_from_file_location("repository", SCRIPT)
INPUT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(INPUT)

# A stand-in `gh` answering `gh repo view <slug>` from this table.
FAKE_GH = """#!{python}
import json, sys
VISIBILITY = {{"acme/public": "PUBLIC", "acme/private": "PRIVATE", "acme/internal": "INTERNAL"}}
slug = sys.argv[3] if len(sys.argv) > 3 and not sys.argv[3].startswith("--") else "acme/private"
if slug not in VISIBILITY:
    sys.exit("GraphQL: Could not resolve to a Repository")
print(json.dumps({{"nameWithOwner": slug, "visibility": VISIBILITY[slug]}}))
"""


def consult(command, cwd=None):
    artifact = {"tool": "host/claude-code/Bash", "arguments": {"command": command}}
    if cwd is not None:
        artifact["cwd"] = cwd
    return {"version": 1, "kind": "input", "name": "claude-code.repository", "declaration": {}, "artifact": artifact}


def slugs(*names):
    return [("slug", name, None) for name in names]


class RepositoryTargets(unittest.TestCase):
    def test_a_gh_call_chooses_its_repository_by_flag_or_environment(self):
        for command in (
            "gh pr create --repo acme/api --title x",
            "gh issue list -R acme/api",
            "gh issue list -Racme/api",
            "gh issue list -R=acme/api",
            "gh pr create --repo=acme/api",
            "GH_REPO=acme/api gh pr create",
            "export GH_REPO=acme/api; gh pr create",
            "sudo GH_REPO=acme/api gh pr create",
        ):
            self.assertEqual(INPUT.repository_targets(command, None), slugs("acme/api"), command)

    def test_an_api_path_or_url_counts_beside_the_checkout(self):
        for command in (
            "gh api repos/acme/api/issues -f title=x",
            "gh api /repos/acme/api/pulls",
            "gh api https://api.github.com/repos/acme/api/pulls",
            "gh issue comment https://github.com/acme/api/issues/5 --body x",
            "gh pr create --title repos/acme/api",
            "gh repo view acme/api",
            "gh repo edit acme/api --description x",
        ):
            self.assertEqual(INPUT.repository_targets(command, "/w"), [("slug", "acme/api", "/w"), ("default", None, "/w")], command)

    def test_a_push_names_a_url_or_a_remote_in_its_directory(self):
        self.assertEqual(INPUT.repository_targets("git push https://github.com/acme/api.git main", None), slugs("acme/api"))
        self.assertEqual(INPUT.repository_targets("git push git@github.com:acme/api.git", None), slugs("acme/api"))
        for command in (
            "cd /w && git push -u upstream feat 2>&1 | tail -2",
            "  git push upstream",
            "git status\ngit push upstream",
            "sudo git push upstream",
            "git push -o ci.skip upstream main",
            "git --no-pager push upstream",
        ):
            self.assertEqual(INPUT.repository_targets(command, "/w"), [("remote", "upstream", "/w")], command)
        self.assertEqual(INPUT.repository_targets("git -C sub push origin", "/w"), [("remote", "origin", "/w/sub")])
        self.assertEqual(INPUT.repository_targets("git -C /other push origin", "/w"), [("remote", "origin", "/other")])
        self.assertEqual(INPUT.repository_targets("git -Csub push origin", "/w"), [("remote", "origin", "/w/sub")])

    def test_a_cd_moves_every_later_call_to_its_directory(self):
        self.assertEqual(INPUT.repository_targets("cd sub && git push origin", "/w"), [("remote", "origin", "/w/sub")])
        self.assertEqual(INPUT.repository_targets("cd /other; gh pr create", "/w"), [("default", None, "/other")])

    def test_every_destination_of_a_compound_command_is_named(self):
        command = "git push https://github.com/acme/public.git && gh pr create --repo acme/private"
        self.assertEqual(INPUT.repository_targets(command, None), slugs("acme/public", "acme/private"))

    def test_words_that_only_look_like_targets_name_nothing(self):
        for command in (
            "git push",
            "gh pr create --draft",
            "ls -R src && git push",
            "gh api repos/{owner}/{repo}/pulls",
            'gh pr create --body "see https://github.com/acme/public"',
            "git -c color.ui=never status && git push",
            "git commit -m \"$(cat <<'EOF'\nfix the parser\nEOF\n)\" && git push",
        ):
            self.assertEqual(INPUT.repository_targets(command, "/w"), [("default", None, "/w")], command)

    def test_a_call_this_input_cannot_follow_is_refused(self):
        for command in (
            'bash -c "git push https://github.com/acme/public.git"',
            "sudo -u bob git push upstream",
            "bash deploy.sh && git push",
            "dash -c 'git push https://github.com/acme/public.git'",
            "fish -c 'gh pr create --repo acme/public'",
            "sh -s < push.sh; gh pr create",
            "V=git; eval $V push https://github.com/acme/public.git",
            'V="git push https://github.com/acme/public.git"; sh -c "$V"',
            "source push.sh; gh pr create",
            "/usr/bin/gi? push https://github.com/acme/public.git",
            "HOME=/tmp/evil git push origin",
            "export XDG_CONFIG_HOME=/tmp/evil; git push origin",
            "GIT_COMMON_DIR=/tmp/other.git git push origin",
            "PATH=/tmp/bin:$PATH gh pr create",
            "gh api --hostname ghe.example repos/acme/api",
            "python3 -c \"import os; os.system('git push https://github.com/acme/public.git')\"",
            "node -e \"require('child_process').execSync('gh pr create --repo acme/public')\"",
            "(cd ../public && git push origin)",
            "cd && git push origin",
            "cd - && git push origin",
            "popd && git push origin",
            "GH_REPO=acme/secret; gh pr create",
            "declare -x GIT_DIR=/tmp/evil; git push origin",
            "printf -v GIT_DIR /tmp/evil; git push origin",
            "xargs git push",
            "git -c url.x.insteadOf=y push origin",
            "git --git-dir=/tmp/x push origin",
            "git --git-dir /tmp/x push origin",
            "GIT_DIR=/tmp/x git push origin",
            "git push $(cat remote.txt)",
            "gh pr create --repo `cat repo.txt`",
            "git push 'unterminated",
            'echo "$(gh pr create --repo acme/public)"',
            "echo `git push https://github.com/acme/public.git`",
            "G=git; $G push https://github.com/acme/public.git",
            "GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=url.x.insteadOf GIT_CONFIG_VALUE_0=y git push origin",
            "GIT_SSH_COMMAND=proxy git push origin",
            "GH_HOST=ghe.example gh pr create --repo acme/api",
            "GH_TOKEN=other gh pr create --repo acme/api",
        ):
            with self.assertRaises(INPUT.Unfollowable, msg=command):
                INPUT.repository_targets(command, "/w")


class RepositoryOf(unittest.TestCase):
    def test_a_call_without_a_command_or_a_directory_is_an_unestablished_finding(self):
        self.assertIsNone(INPUT.repository_of(consult("gh pr create --draft"))["visibility"])
        finding = INPUT.repository_of({"version": 1, "kind": "input", "artifact": {"tool": "Read", "arguments": {"file_path": "x"}}})
        self.assertIsNone(finding["visibility"])

    def test_a_directory_that_is_no_checkout_is_an_unestablished_finding(self):
        with tempfile.TemporaryDirectory() as empty:
            finding = INPUT.repository_of(consult("git push origin main", cwd=empty))
        self.assertIsNone(finding["visibility"])
        self.assertTrue(finding["reason"])

    def test_an_unfollowable_call_is_an_unestablished_finding(self):
        self.assertIsNone(INPUT.repository_of(consult('bash -c "git push x"', cwd="/"))["visibility"])

    def test_an_option_named_as_the_repository_is_an_unestablished_finding(self):
        self.assertIsNone(INPUT.repository_of(consult("gh pr create --repo --jq", cwd="/"))["visibility"])

    def test_too_many_destinations_are_an_unestablished_finding(self):
        command = "; ".join(f"gh pr create --repo acme/r{index}" for index in range(INPUT.MAX_TARGETS + 1))
        self.assertIsNone(INPUT.repository_of(consult(command, cwd="/"))["visibility"])

    def test_a_consult_of_another_kind_is_refused(self):
        with self.assertRaises(ValueError):
            INPUT.repository_of({"version": 1, "kind": "annotation", "artifact": {"args": {}}})


class Program(unittest.TestCase):
    def run_program(self, command, path=None):
        env = {**os.environ, "PATH": path} if path else None
        completed = subprocess.run(
            [sys.executable, str(SCRIPT)],
            input=json.dumps(consult(command)),
            capture_output=True,
            text=True,
            check=False,
            env=env,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        answer = json.loads(completed.stdout)
        self.assertEqual(answer["version"], 1)
        return answer["answer"]

    def with_fake_gh(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        gh = Path(directory.name) / "gh"
        gh.write_text(FAKE_GH.format(python=sys.executable))
        gh.chmod(0o755)
        return f"{directory.name}:{os.environ['PATH']}"

    def test_the_program_answers_the_envelope_with_a_zero_exit(self):
        self.assertIsNone(self.run_program("gh pr create --draft")["visibility"])

    def test_several_destinations_answer_the_most_widely_readable(self):
        path = self.with_fake_gh()
        finding = self.run_program("git push https://github.com/acme/public.git && gh pr create --repo acme/private", path)
        self.assertEqual(finding, {"name_with_owner": "acme/public", "visibility": "public"})
        finding = self.run_program("gh pr create --repo acme/private; gh issue list -R acme/internal", path)
        self.assertEqual(finding["visibility"], "internal")

    def test_one_unestablished_destination_makes_the_finding_unestablished(self):
        path = self.with_fake_gh()
        self.assertIsNone(self.run_program("gh pr create --repo acme/private && gh pr create --repo acme/gone", path)["visibility"])

    def test_an_oversized_or_foreign_consult_exits_nonzero(self):
        for body in ("x" * (INPUT.MAX_INPUT_BYTES + 1), json.dumps({"version": 2})):
            completed = subprocess.run([sys.executable, str(SCRIPT)], input=body, capture_output=True, text=True, check=False)
            self.assertNotEqual(completed.returncode, 0)


if __name__ == "__main__":
    unittest.main()
