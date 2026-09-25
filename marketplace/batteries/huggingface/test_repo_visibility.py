from http.server import BaseHTTPRequestHandler, HTTPServer
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import unittest


SCRIPT = Path(__file__).with_name("repo-visibility.py")
SPEC = importlib.util.spec_from_file_location("repo_visibility", SCRIPT)
ANNOTATOR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ANNOTATOR)

GROUP = "@huggingface:org/acme/resource-group/507f1f77bcf86cd799439011/members"
GROUP_BY_NAME = "@huggingface:org/acme/resource-group/research/members"
DECLARED = ["self", GROUP]

# Recorded Hub payloads (2026-09-15), trimmed to the fields the script reads.
WHOAMI = {"type": "user", "name": "arsenyinfo", "email": "me@arseny.info", "emailVerified": True, "orgs": []}
PUBLIC_MODEL = {"id": "openai-community/gpt2", "author": "openai-community", "private": False, "gated": False}
GATED_MODEL = {"id": "meta-llama/Llama-3.1-8B", "author": "meta-llama", "private": False, "gated": "manual"}
PUBLIC_DATASET = {"id": "HuggingFaceFW/fineweb", "author": "HuggingFaceFW", "private": False, "gated": False}
PUBLIC_SPACE = {"id": "mcp-tools/Z-Image-Turbo", "author": "mcp-tools", "private": False, "gated": False}
NOT_FOUND = {"error": "Repository not found"}
# Shapes the test token cannot produce, following the Hub's OpenAPI schema.
OWN_PRIVATE = {"id": "arsenyinfo/notes", "author": "arsenyinfo", "private": True, "gated": False}
ORG_PRIVATE = {"id": "acme/weights", "author": "acme", "private": True, "gated": False}
ORG_PRIVATE_UNGROUPED = {"id": "acme/other", "author": "acme", "private": True, "gated": False}
RESOURCE_GROUPS = [
    {
        "id": "507f1f77bcf86cd799439011",
        "name": "research",
        "users": [{"name": "alice", "role": "read"}, {"name": "mallory", "role": "no_access"}],
        "resources": [{"type": "model", "name": "acme/weights", "private": True}],
    }
]

HUB = {
    "/api/whoami-v2": WHOAMI,
    "/api/models/openai-community/gpt2": PUBLIC_MODEL,
    "/api/models/meta-llama/Llama-3.1-8B": GATED_MODEL,
    "/api/datasets/HuggingFaceFW/fineweb": PUBLIC_DATASET,
    "/api/spaces/mcp-tools/Z-Image-Turbo": PUBLIC_SPACE,
    "/api/models/arsenyinfo/notes": OWN_PRIVATE,
    "/api/models/acme/weights": ORG_PRIVATE,
    "/api/models/acme/other": ORG_PRIVATE_UNGROUPED,
    "/api/organizations/acme/resource-groups": RESOURCE_GROUPS,
    "/api/collections/acme/papers": {"slug": "acme/papers", "private": True},
    "/api/collections/openai-community/models": {"slug": "openai-community/models", "private": False},
}


def fixture_hub(answers=HUB, **extra):
    """A Hub answering from recorded payloads; a path it lacks is a 404."""
    table = {**answers, **extra}
    seen = []

    def call(path):
        seen.append(path)
        match table.get(path):
            case None:
                raise ANNOTATOR.NotFound(path)
            case Exception() as error:
                raise error
            case payload:
                return payload

    hub = ANNOTATOR.Hub(call)
    hub.seen = seen
    return hub


def consult(name=ANNOTATOR.CONTENT, tool="hub_repo_details", arguments=None, declared=DECLARED, **overrides):
    return {
        "version": 1,
        "kind": "annotation",
        "name": name,
        "declaration": {"trust_ranks": ["suspicious"], "audiences": declared, "attention_marks": [], "effects": []},
        "artifact": {"args": {"name": f"mcp/huggingface/{tool}", "arguments": arguments or {"repo_ids": ["openai-community/gpt2"]}}},
        **overrides,
    }


def details(*repo_ids, **arguments):
    return {"repo_ids": list(repo_ids), **arguments}


def fs(*operations):
    return {"operations": [{"cmd": cmd, "args": list(args)} for cmd, *args in operations]}


def read(hub, arguments, tool="hub_repo_details", declared=DECLARED):
    return ANNOTATOR.annotation(hub, ANNOTATOR.CONTENT, tool, arguments, declared)


def write(hub, tool, arguments, declared=DECLARED):
    return ANNOTATOR.annotation(hub, ANNOTATOR.READERS, tool, arguments, declared)


class GrammarTests(unittest.TestCase):
    def test_a_repository_id_drops_its_revision_and_keeps_one_segment_each(self):
        self.assertEqual(ANNOTATOR.parse_repo_id("acme/weights@v2"), ANNOTATOR.Repo(None, "acme", "weights"))
        self.assertEqual(ANNOTATOR.parse_repo_id("acme/weights", "model"), ANNOTATOR.Repo("model", "acme", "weights"))
        for text in ["weights", "acme/a/b", "/weights", "acme/", "$owner/x", "acme/../x", ".hidden/x", 7, None]:
            with self.assertRaises(ValueError, msg=repr(text)):
                ANNOTATOR.parse_repo_id(text)

    def test_a_path_segment_is_percent_encoded(self):
        self.assertEqual(ANNOTATOR.Repo("model", "acme", "w.1").path("model"), "/api/models/acme/w.1")
        self.assertEqual(ANNOTATOR.quote("a b"), "a%20b")

    def test_hf_uri_classes(self):
        public = ANNOTATOR.Fixed(ANNOTATOR.PUBLIC)
        viewer = ANNOTATOR.Fixed(ANNOTATOR.SELF)
        self.assertEqual(ANNOTATOR.parse_uri("hf://"), public)
        self.assertEqual(ANNOTATOR.parse_uri("hf://papers"), public)
        self.assertEqual(ANNOTATOR.parse_uri("hf://papers/2306.01116"), public)
        self.assertEqual(ANNOTATOR.parse_uri("hf://docs/hub/security"), public)
        for uri in ["hf://models", "hf://datasets", "hf://spaces", "hf://collections", "hf://models/acme", "hf://collections/acme"]:
            self.assertEqual(ANNOTATOR.parse_uri(uri), viewer, uri)
        self.assertEqual(
            ANNOTATOR.parse_uri("hf://models/acme/weights@v2/config.json"),
            ANNOTATOR.Repo("model", "acme", "weights"),
        )
        self.assertEqual(ANNOTATOR.parse_uri("hf://datasets/acme/corpus"), ANNOTATOR.Repo("dataset", "acme", "corpus"))
        self.assertEqual(ANNOTATOR.parse_uri("hf://collections/acme/papers/items"), ANNOTATOR.Collection("acme", "papers"))
        for uri in ["hf://buckets/acme/data", "hf://kernels/x", "hf://models//weights", "hf://models/acme/$name", "s3://x", "", None, 3]:
            with self.assertRaises(ValueError, msg=repr(uri)):
                ANNOTATOR.parse_uri(uri)

    def test_a_create_repo_uri_is_exactly_one_repository(self):
        self.assertEqual(ANNOTATOR.parse_repo_uri("hf://spaces/acme/demo", "uri"), ANNOTATOR.Repo("space", "acme", "demo"))
        for uri in ["hf://models/acme/weights/config.json", "hf://models/acme/weights@v2", "hf://models/acme", "hf://buckets/acme/x", "hf://papers", "hf://collections/acme/papers"]:
            with self.assertRaises(ValueError, msg=uri):
                ANNOTATOR.parse_repo_uri(uri, "uri")

    def test_more_entries_than_the_tool_admits_are_refused_before_any_lookup(self):
        hub = fixture_hub()
        with self.assertRaises(ValueError):
            read(hub, details(*[f"acme/m{i}" for i in range(11)]))
        with self.assertRaises(ValueError):
            read(hub, fs(*[("stat", "hf://models/acme/weights")] * 31), tool="hf_fs")
        with self.assertRaises(ValueError):
            read(hub, details())
        self.assertEqual(hub.seen, [])


class ReadTests(unittest.TestCase):
    def test_a_public_repository_is_suspicious_and_public(self):
        answer = read(fixture_hub(), details("openai-community/gpt2", repo_type="model"))
        self.assertEqual(
            answer,
            {"delta": {"trust": "suspicious", "audience": "public"}, "requires": {"history": [], "attention": []}, "emits": []},
        )

    def test_without_a_type_every_type_the_name_exists_as_is_folded(self):
        hub = fixture_hub()
        read(hub, details("openai-community/gpt2"))
        self.assertEqual(
            sorted(path for path in hub.seen if "/gpt2" in path),
            ["/api/datasets/openai-community/gpt2", "/api/models/openai-community/gpt2", "/api/spaces/openai-community/gpt2"],
        )
        with self.assertRaises(ANNOTATOR.NotFound):
            read(fixture_hub(), details("nobody/nothing"))

    def test_a_named_type_that_does_not_exist_is_refused(self):
        with self.assertRaises(ANNOTATOR.NotFound):
            read(fixture_hub(), details("openai-community/gpt2", repo_type="dataset"))

    def test_gated_metadata_is_public_but_gated_files_stay_with_the_viewer(self):
        overview = read(fixture_hub(), details("meta-llama/Llama-3.1-8B", repo_type="model"))
        self.assertEqual(overview["delta"]["audience"], "public")
        preview = read(fixture_hub(), details("meta-llama/Llama-3.1-8B", repo_type="model", operations=["dataset_preview"]))
        self.assertEqual(preview["delta"]["audience"], ["self"])
        listing = read(fixture_hub(), fs(("ls", "hf://models/meta-llama/Llama-3.1-8B")), tool="hf_fs")
        self.assertEqual(listing["delta"]["audience"], "public")
        for command in ["cat", "attach"]:
            content = read(fixture_hub(), fs((command, "hf://models/meta-llama/Llama-3.1-8B/config.json")), tool="hf_fs")
            self.assertEqual(content["delta"]["audience"], ["self"], command)

    def test_the_viewers_own_private_repository_stays_with_the_viewer(self):
        answer = read(fixture_hub(), details("arsenyinfo/notes", repo_type="model"))
        self.assertEqual(answer["delta"]["audience"], ["self"])

    def test_a_private_org_repository_in_a_declared_resource_group_is_read_by_the_group(self):
        answer = read(fixture_hub(), details("acme/weights", repo_type="model"))
        self.assertEqual(answer["delta"]["audience"], [GROUP])
        by_name = read(fixture_hub(), details("acme/weights", repo_type="model"), declared=["self", GROUP_BY_NAME])
        self.assertEqual(by_name["delta"]["audience"], [GROUP_BY_NAME])

    def test_a_private_org_repository_falls_back_to_the_viewer(self):
        # No group is declared: the answer is the viewer, and the Hub is
        # asked about neither the viewer nor the organization's groups.
        hub = fixture_hub()
        self.assertEqual(read(hub, details("acme/weights", repo_type="model"), declared=["self"])["delta"]["audience"], ["self"])
        self.assertEqual(hub.seen, ["/api/models/acme/weights"])
        # The repository is in no group the token can see.
        self.assertEqual(read(fixture_hub(), details("acme/other", repo_type="model"))["delta"]["audience"], ["self"])
        # The listing is unreadable or malformed: the repository lookup already attested the viewer reads it.
        for listing in [ANNOTATOR.Forbidden("groups"), RuntimeError("500"), {"not": "a list"}, [{"resources": "x"}]]:
            hub = fixture_hub(**{"/api/organizations/acme/resource-groups": listing})
            self.assertEqual(read(hub, details("acme/weights", repo_type="model"))["delta"]["audience"], ["self"], repr(listing))

    def test_a_group_whose_name_is_no_path_segment_is_matched_by_id_only(self):
        listing = [{**RESOURCE_GROUPS[0], "name": "Cohort 2024"}]
        hub = fixture_hub(**{"/api/organizations/acme/resource-groups": listing})
        self.assertEqual(read(hub, details("acme/weights", repo_type="model"))["delta"]["audience"], [GROUP])
        hub = fixture_hub(**{"/api/organizations/acme/resource-groups": listing})
        spelled = "@huggingface:org/acme/resource-group/Cohort 2024/members"
        self.assertEqual(read(hub, details("acme/weights", repo_type="model"), declared=["self", spelled])["delta"]["audience"], ["self"])

    def test_a_batch_folds_to_the_narrowest_nameable_audience(self):
        hub = fixture_hub()
        public = read(hub, details("openai-community/gpt2", "meta-llama/Llama-3.1-8B", repo_type="model"))
        self.assertEqual(public["delta"]["audience"], "public")
        group = read(hub, details("openai-community/gpt2", "acme/weights", repo_type="model"))
        self.assertEqual(group["delta"]["audience"], [GROUP])
        mixed = read(hub, details("arsenyinfo/notes", "acme/weights", repo_type="model"))
        self.assertEqual(mixed["delta"]["audience"], ["self"])
        other = "@huggingface:org/acme/resource-group/other/members"
        other_group = {"id": "other", "name": "other", "resources": [{"type": "model", "name": "acme/other", "private": True}]}
        hub = fixture_hub(**{"/api/organizations/acme/resource-groups": [*RESOURCE_GROUPS, other_group]})
        two = read(hub,details("acme/weights", "acme/other", repo_type="model"), declared=["self", GROUP, other])
        self.assertEqual(two["delta"]["audience"], ["self"])
        with self.assertRaises(ANNOTATOR.NotFound):
            read(fixture_hub(), details("openai-community/gpt2", "nobody/nothing", repo_type="model"))

    def test_hf_fs_classes_and_collections(self):
        hub = fixture_hub()
        answer = read(hub, fs(("ls", "hf://papers"), ("cat", "hf://docs/hub/security")), tool="hf_fs")
        self.assertEqual(answer["delta"]["audience"], "public")
        self.assertEqual(hub.seen, [])
        answer = read(fixture_hub(), fs(("ls", "hf://models"), ("stat", "hf://datasets/HuggingFaceFW/fineweb")), tool="hf_fs")
        self.assertEqual(answer["delta"]["audience"], ["self"])
        self.assertEqual(read(fixture_hub(), fs(("ls", "hf://collections/acme/papers")), tool="hf_fs")["delta"]["audience"], ["self"])
        self.assertEqual(read(fixture_hub(), fs(("ls", "hf://collections/openai-community/models")), tool="hf_fs")["delta"]["audience"], "public")
        with self.assertRaises(ANNOTATOR.NotFound):
            read(fixture_hub(), fs(("ls", "hf://collections/nobody/nothing")), tool="hf_fs")
        for operations in [fs(("ls", "hf://buckets/acme/data")), fs(("cat",)), {"operations": ["hf://models"]}, {"operations": [{"cmd": "ls"}]}]:
            with self.assertRaises(ValueError, msg=repr(operations)):
                read(fixture_hub(), operations, tool="hf_fs")

    def test_a_search_stays_with_the_viewer(self):
        answer = read(fixture_hub(), fs(("search", "hf://models", "llama")), tool="hf_fs")
        self.assertEqual(answer, {"delta": {"trust": "suspicious", "audience": ["self"]}, "requires": {"history": [], "attention": []}, "emits": []})

    def test_the_hub_must_report_a_visibility(self):
        for payload in [{"id": "acme/x"}, {"private": "no"}, [], "text", {"private": False, "gated": "sometimes"}]:
            hub = fixture_hub(**{"/api/models/acme/x": payload})
            with self.assertRaises(RuntimeError, msg=repr(payload)):
                read(hub, details("acme/x", repo_type="model"))
        for error in [ANNOTATOR.Forbidden("x"), RuntimeError("503")]:
            with self.assertRaises(type(error)):
                read(fixture_hub(**{"/api/models/acme/x": error}), details("acme/x", repo_type="model"))

    def test_a_read_consult_refuses_write_tools_and_unknown_types(self):
        with self.assertRaises(ValueError):
            read(fixture_hub(), {"args": ["hf://models/acme/weights/x"]}, tool="hf_fs_write")
        with self.assertRaises(ValueError):
            read(fixture_hub(), details("acme/weights", repo_type="bucket"))
        with self.assertRaises(ValueError):
            read(fixture_hub(), details("meta-llama/Llama-3.1-8B", repo_type="model", operations="dataset_preview"))


class WriteTests(unittest.TestCase):
    def test_a_file_write_needs_trusted_data_everyone_the_repository_reaches_may_see(self):
        public = write(fixture_hub(), "hf_fs_write", {"cmd": "put", "args": ["hf://models/openai-community/gpt2/README.md"], "content": "x"})
        self.assertEqual(
            public,
            {"delta": {}, "requires": {"trust": "trusted", "audience": {"contains": "public"}, "history": [], "attention": []}, "emits": []},
        )
        own = write(fixture_hub(), "hf_fs_write", {"cmd": "rm", "args": ["hf://models/arsenyinfo/notes/old.txt"]})
        self.assertEqual(own["requires"]["audience"], {"contains": ["self"]})
        # An organization's admins read every private repository; no collection lists them.
        org = write(fixture_hub(), "hf_fs_write", {"cmd": "put", "args": ["hf://models/acme/weights/README.md"], "content": "x"})
        self.assertEqual(org["requires"]["audience"], {"contains": "public"})
        for args in [["hf://models/acme"], ["hf://buckets/acme/x/f"], ["hf://papers"], [], "hf://models/acme/weights/f"]:
            with self.assertRaises(ValueError, msg=repr(args)):
                write(fixture_hub(), "hf_fs_write", {"cmd": "put", "args": args})
        with self.assertRaises(ANNOTATOR.NotFound):
            write(fixture_hub(), "hf_fs_write", {"cmd": "put", "args": ["hf://models/nobody/nothing/f"]})

    def test_a_new_repository_needs_data_its_readers_may_see(self):
        hub = fixture_hub()
        omitted = write(hub, "create_repo", {"uri": "hf://models/arsenyinfo/new"})
        self.assertEqual(omitted, {"delta": {}, "requires": {"trust": "trusted", "audience": {"contains": "public"}, "history": [], "attention": []}, "emits": []})
        # A public target reaches everyone whoever the viewer is: the Hub is not asked.
        self.assertEqual(hub.seen, [])
        own = write(hub, "create_repo", {"uri": "hf://models/arsenyinfo/new", "private": True})
        self.assertEqual(own["requires"]["audience"], {"contains": ["self"]})
        org = write(hub, "create_repo", {"uri": "hf://models/acme/new", "private": True})
        self.assertEqual(org["requires"]["audience"], {"contains": "public"})
        self.assertEqual([path for path in hub.seen if "/new" in path], [])
        for arguments in [{"uri": "hf://buckets/acme/x"}, {"uri": "hf://models/acme"}, {"uri": "hf://models/acme/new", "private": "yes"}, {}]:
            with self.assertRaises(ValueError, msg=repr(arguments)):
                write(fixture_hub(), "create_repo", arguments)

    def test_a_copy_is_admitted_only_when_the_source_readers_are_inside_the_targets(self):
        admitted = [
            ({"uri": "hf://models/arsenyinfo/fork", "source_uri": "hf://models/openai-community/gpt2"}, "public"),
            ({"uri": "hf://models/arsenyinfo/fork", "source_uri": "hf://models/openai-community/gpt2", "private": True}, ["self"]),
            ({"uri": "hf://models/arsenyinfo/copy", "source_uri": "hf://models/arsenyinfo/notes"}, ["self"]),
        ]
        for arguments, target in admitted:
            answer = write(fixture_hub(), "create_repo", arguments)
            self.assertEqual(answer, {"delta": {}, "requires": {"trust": "trusted", "audience": {"contains": target}, "history": [], "attention": []}, "emits": []}, repr(arguments))
        refused = [
            {"uri": "hf://models/arsenyinfo/copy", "source_uri": "hf://models/arsenyinfo/notes", "private": False},
            {"uri": "hf://models/acme/copy", "source_uri": "hf://models/arsenyinfo/notes"},
            {"uri": "hf://models/arsenyinfo/copy", "source_uri": "hf://models/acme/weights"},
            {"uri": "hf://models/acme/copy", "source_uri": "hf://models/acme/weights"},
            {"uri": "hf://models/arsenyinfo/copy", "source_uri": "hf://models/meta-llama/Llama-3.1-8B"},
        ]
        for arguments in refused:
            with self.assertRaises(RuntimeError, msg=repr(arguments)):
                write(fixture_hub(), "create_repo", arguments)
        with self.assertRaises(ValueError):
            write(fixture_hub(), "create_repo", {"uri": "hf://datasets/arsenyinfo/copy", "source_uri": "hf://models/openai-community/gpt2"})

    def test_a_write_consult_refuses_read_tools_and_static_tools(self):
        for tool, arguments in [("hub_repo_details", details("openai-community/gpt2")), ("dynamic_space", {"operation": "invoke", "space_name": "a/b"}), ("hf_jobs", {})]:
            with self.assertRaises(ValueError, msg=tool):
                write(fixture_hub(), tool, arguments)


class ConsultTests(unittest.TestCase):
    def test_the_tool_and_arguments_are_read_from_the_call(self):
        self.assertEqual(ANNOTATOR.call_of(consult()), (ANNOTATOR.CONTENT, "hub_repo_details", {"repo_ids": ["openai-community/gpt2"]}))
        self.assertEqual(ANNOTATOR.call_of(consult(name=ANNOTATOR.READERS, tool="hf_fs_write"))[:2], (ANNOTATOR.READERS, "hf_fs_write"))

    def test_a_foreign_consult_is_refused(self):
        for request in [
            consult(version=2),
            consult(kind="audience"),
            consult(name="huggingface.other"),
            consult(artifact={}),
            consult(artifact={"args": {"arguments": {}}}),
            consult(artifact={"args": {"name": "mcp/huggingface/hf_fs", "arguments": []}}),
            [],
        ]:
            with self.assertRaises(ValueError):
                ANNOTATOR.call_of(request)


class Loopback:
    """One stdlib HTTP server standing in for the Hub."""

    def __init__(self, answers):
        seen = []

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                seen.append((self.path, self.headers.get("Authorization")))
                match answers.get(self.path):
                    case None:
                        body, status = json.dumps(NOT_FOUND).encode(), 404
                    case payload:
                        body, status = json.dumps(payload).encode(), 200
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *_):
                pass

        self.seen = seen
        self.server = HTTPServer(("127.0.0.1", 0), Handler)

    def __enter__(self):
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        return self

    def __exit__(self, *_):
        self.server.shutdown()
        self.server.server_close()

    def env(self):
        # The trailing slash is dropped by the script, not by this server.
        return {"PATH": "/usr/bin:/bin", "APPA_PROVIDER_HUGGINGFACE_TOKEN": "hf-fixture", "HF_ENDPOINT": f"http://127.0.0.1:{self.server.server_port}/"}


class EnvelopeTests(unittest.TestCase):
    def run_script(self, request, env):
        return subprocess.run([sys.executable, str(SCRIPT)], input=json.dumps(request), capture_output=True, text=True, env=env)

    def test_the_hub_root_is_hf_endpoint_and_the_token_is_sent(self):
        with Loopback(HUB) as hub:
            result = self.run_script(consult(arguments=details("acme/weights", repo_type="model")), hub.env())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(hub.seen[0], ("/api/models/acme/weights", "Bearer hf-fixture"))
        self.assertEqual(json.loads(result.stdout), {"version": 1, "answer": {"delta": {"trust": "suspicious", "audience": [GROUP]}, "requires": {"history": [], "attention": []}, "emits": []}})

    def test_a_repository_the_token_cannot_see_is_a_failure_not_a_guess(self):
        with Loopback(HUB) as hub:
            result = self.run_script(consult(arguments=details("nobody/nothing", repo_type="model")), hub.env())
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(result.stdout, "")

    def test_a_missing_token_is_a_failure_before_any_network(self):
        with tempfile.TemporaryDirectory() as home:
            result = self.run_script(consult(), {"PATH": "/usr/bin:/bin", "HOME": home})
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("APPA_PROVIDER_HUGGINGFACE_TOKEN", result.stderr)
        self.assertEqual(result.stdout, "")

    def test_a_mandate_without_self_is_refused_before_the_token_is_read(self):
        for audiences in [[GROUP], ["internal"], [], None]:
            request = consult(declaration={"trust_ranks": ["suspicious"], "audiences": audiences})
            result = self.run_script(request, {"PATH": "/usr/bin:/bin"})
            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertEqual(result.stdout, "")

    def test_an_oversized_consult_is_refused(self):
        request = consult(name=ANNOTATOR.READERS, tool="hf_fs_write", arguments={"cmd": "put", "args": ["hf://models/a/b/f"], "content": "x" * (ANNOTATOR.MAX_INPUT_BYTES + 1)})
        with Loopback(HUB) as hub:
            result = self.run_script(request, hub.env())
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertEqual(hub.seen, [])


if __name__ == "__main__":
    unittest.main()
