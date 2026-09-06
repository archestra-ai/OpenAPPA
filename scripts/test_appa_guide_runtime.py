from __future__ import annotations

import importlib.util
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

import tomllib

SCRIPT = Path(__file__).with_name("appa-guide-runtime.py")
SPEC = importlib.util.spec_from_file_location("appa_guide_runtime", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
guide = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(guide)


class AppaGuideRuntimeTests(unittest.TestCase):
    def test_runtime_state_reports_policy_key_includes_and_storage(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config = root / "appa.toml"
            config.write_text(
                'include = ["batteries/github/appa.toml"]\n[policy]\nversion = 2\n',
                encoding="utf-8",
            )
            identity = root / "identity"
            identity.mkdir()
            (identity / "namespace").write_text("appa\n", encoding="utf-8")
            with (
                mock.patch.object(guide, "IDENTITY_DIR", identity),
                mock.patch.object(guide, "POLICY_CONFIGMAP", "appa-runtime-policy"),
                mock.patch.object(guide, "RUNTIME_RELEASE", "appa-runtime"),
                mock.patch.object(guide, "runtime_request", return_value=b"policy-key"),
                mock.patch.object(
                    guide, "refresh_state", return_value={"supported": True}
                ),
                mock.patch.dict(guide.os.environ, {"APPA_CONFIG": str(config)}),
            ):
                state = guide.runtime_state()
        self.assertEqual(state["policy_key"], "policy-key")
        self.assertEqual(state["included_batteries"], ["batteries/github/appa.toml"])
        self.assertEqual(state["policy_configmap"]["name"], "appa-runtime-policy")
        self.assertTrue(state["battery_refresh"]["supported"])

    def test_apply_annotation_accepts_only_complete_agents(self) -> None:
        def consult(manifest: str) -> dict:
            return {
                "version": 1,
                "kind": "annotation",
                "name": "appa-guide-apply",
                "artifact": {"args": {"arguments": {"manifest": manifest}}},
            }

        agent = "apiVersion: kagent.dev/v1alpha2\nkind: Agent\nmetadata:\n  name: fixture\nspec:\n  type: Declarative\n"
        self.assertEqual(
            guide.annotate_apply(consult(agent))["answer"]["requires"]["attention"],
            ["human-approval"],
        )
        with self.assertRaisesRegex(ValueError, "complete spec and no status"):
            guide.annotate_apply(consult(agent + "status:\n  conditions: []\n"))
        configmap = "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: policy\ndata:\n  appa.toml: |\n    [policy]\n"
        with self.assertRaisesRegex(ValueError, "supports only Agent"):
            guide.annotate_apply(consult(configmap))
        smuggled = (
            agent + "---\napiVersion: v1\nkind: Secret\nmetadata:\n  name: stolen\n"
        )
        with self.assertRaisesRegex(ValueError, "exactly one Agent document"):
            guide.annotate_apply(consult(smuggled))
        mixed = "apiVersion: kagent.dev/v1alpha2\nkind: Agent\nkind: Secret\nspec:\n  type: Declarative\n"
        with self.assertRaisesRegex(ValueError, "supports only Agent"):
            guide.annotate_apply(consult(mixed))

    def test_battery_include_preserves_the_complete_root(self) -> None:
        root = (
            '[policy]\nversion = 2\n[[policy.tool]]\nname = "list_pods"\ndelta = {}\n'
        )
        added, changed = guide.with_battery_include(root, "github")
        self.assertTrue(changed)
        self.assertTrue(
            added.startswith('include = ["batteries/github/appa.toml"]\n\n')
        )
        self.assertIn(root, added)

        expanded, changed = guide.with_battery_include(
            'include = [\n  "batteries/slack/appa.toml",\n]\n' + root,
            "github",
        )
        self.assertTrue(changed)
        self.assertEqual(
            tomllib.loads(expanded)["include"],
            ["batteries/slack/appa.toml", "batteries/github/appa.toml"],
        )
        unchanged, changed = guide.with_battery_include(expanded, "github")
        self.assertFalse(changed)
        self.assertEqual(unchanged, expanded)

    def test_demo_policy_preserves_the_bootstrap_shape(self) -> None:
        repository = SCRIPT.parent.parent
        bootstrap = tomllib.loads(
            (repository / "charts/appa-runtime/files/appa.toml").read_text()
        )
        demo = tomllib.loads(
            (
                repository / "integrations/kagent/demo/chart/files/demo.appa.toml"
            ).read_text()
        )
        self.assertTrue(guide.preserves_policy_shape(bootstrap, demo))
        self.assertFalse(
            guide.preserves_policy_shape(bootstrap, {"policy": {"version": 2}}),
            "an include-only policy cannot replace the bootstrap",
        )
        current = {
            "policy": {
                "tool": [
                    {"name": "first", "delta": {}},
                    {"name": "second", "delta": {}},
                ]
            }
        }
        reordered = {
            "policy": {
                "tool": [
                    {"name": "second", "delta": {}},
                    {"name": "first", "delta": {}},
                ]
            }
        }
        changed = {
            "policy": {
                "tool": [
                    {"name": "first", "delta": {"trust": "suspicious"}},
                    {"name": "second", "delta": {}},
                ]
            }
        }
        self.assertFalse(guide.preserves_policy_shape(current, reordered))
        self.assertTrue(guide.preserves_policy_shape(current, changed))

    def test_projection_timeout_rolls_back_the_configmap(self) -> None:
        resources = [
            {"metadata": {"resourceVersion": "1"}, "data": {"appa.toml": "old"}},
            {},
            {"metadata": {"resourceVersion": "2"}, "data": {"appa.toml": "new"}},
            {},
        ]
        with (
            mock.patch.object(
                guide, "kube_request", side_effect=resources
            ) as requested,
            mock.patch.object(guide, "wait_for_policy", side_effect=[False, True]),
            mock.patch.object(guide, "configmap_path", return_value="/configmap"),
            self.assertRaisesRegex(RuntimeError, "prior policy restored"),
        ):
            guide.update_policy_configmap("old", "new")
        patches = [
            call.args[2] for call in requested.call_args_list if len(call.args) == 3
        ]
        self.assertEqual(patches[0]["data"]["appa.toml"], "new")
        self.assertEqual(patches[1]["data"]["appa.toml"], "old")

    def test_a_failed_reload_rolls_back_and_names_a_split_brain(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            config = Path(directory) / "appa.toml"
            config.write_text("new", encoding="utf-8")

            def restore(_candidate: str, current: str) -> None:
                config.write_text(current, encoding="utf-8")

            with (
                mock.patch.dict(guide.os.environ, {"APPA_CONFIG": str(config)}),
                mock.patch.object(guide, "update_policy_configmap"),
                mock.patch.object(
                    guide, "runtime_request", side_effect=TimeoutError("reload stalled")
                ),
                mock.patch.object(guide, "restore_serving_policy", side_effect=restore),
                self.assertRaises(TimeoutError),
            ):
                guide.publish_and_reload("old", "new")
            self.assertEqual(config.read_text(encoding="utf-8"), "old")

            config.write_text("new", encoding="utf-8")
            with (
                mock.patch.dict(guide.os.environ, {"APPA_CONFIG": str(config)}),
                mock.patch.object(guide, "update_policy_configmap"),
                mock.patch.object(
                    guide, "runtime_request", side_effect=TimeoutError("reload stalled")
                ),
                mock.patch.object(
                    guide,
                    "restore_serving_policy",
                    side_effect=RuntimeError("rollback reload failed"),
                ),
                self.assertRaisesRegex(
                    RuntimeError, "rollback did not restore serving policy"
                ),
            ):
                guide.publish_and_reload("old", "new")

    def test_refresh_state_reports_the_active_and_previous_layers(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / "release-batteries"
            target.mkdir()
            (target / ".appa-release").write_text("v1.2.3\n", encoding="utf-8")
            (root / ".release-batteries.previous").mkdir()
            with (
                mock.patch.dict(
                    guide.os.environ, {"APPA_BATTERY_REFRESH_TARGET": str(target)}
                ),
                mock.patch.object(guide, "REFRESH", str(root / "missing")),
            ):
                state = guide.refresh_state()
        self.assertEqual(state["release"], "v1.2.3")
        self.assertTrue(state["pending_previous_layer"])
        self.assertFalse(state["supported"])

    def test_refresh_requires_persistent_storage(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            helper = Path(directory) / "refresh"
            helper.write_text("", encoding="utf-8")
            with (
                mock.patch.object(guide, "REFRESH", str(helper)),
                mock.patch.object(guide, "PERSISTENCE_ENABLED", True),
            ):
                self.assertTrue(guide.refresh_state()["supported"])
            with mock.patch.object(guide, "REFRESH", str(helper)):
                self.assertFalse(guide.refresh_state()["supported"])

    def test_mutating_management_operations_are_serialized(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            entered = threading.Event()
            release = threading.Event()
            second_entered = threading.Event()

            def first() -> None:
                with guide.management_lock():
                    entered.set()
                    release.wait(2)

            def second() -> None:
                entered.wait(2)
                with guide.management_lock():
                    second_entered.set()

            with mock.patch.object(
                guide, "MANAGEMENT_LOCK", Path(directory) / "management.lock"
            ):
                first_thread = threading.Thread(target=first)
                second_thread = threading.Thread(target=second)
                first_thread.start()
                second_thread.start()
                self.assertTrue(entered.wait(1))
                time.sleep(0.05)
                self.assertFalse(second_entered.is_set())
                release.set()
                first_thread.join(2)
                second_thread.join(2)
                self.assertTrue(second_entered.is_set())


if __name__ == "__main__":
    unittest.main()
