"""Pin the release charts' image digests the way release.yml does."""

import importlib.util
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("chart_digests", Path(__file__).with_name("appa-chart-digests.py"))
DIGESTS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(DIGESTS)

RUNTIME = "sha256:" + "a" * 64
TOOLS = "sha256:" + "b" * 64
MOCKS = "sha256:" + "c" * 64
PUBLIC = "europe-west1-docker.pkg.dev/friendly-path-465518-r6/appa-public"


def render(chart: Path, values: str) -> str:
    with tempfile.TemporaryDirectory() as directory:
        copy = Path(directory) / chart.name
        subprocess.run(["cp", "-R", str(chart), str(copy)], check=True)
        (copy / "values.yaml").write_text(values)
        return subprocess.run(["helm", "template", "release", str(copy), "--namespace", "kagent"],
                              check=True, capture_output=True, text=True).stdout


class ChartDigestTests(unittest.TestCase):
    def test_runtime_chart_pulls_the_pinned_digest(self):
        chart = ROOT / "charts/appa-runtime"
        values = DIGESTS.pin((chart / "values.yaml").read_text(), {"appa-runtime": RUNTIME})
        self.assertIn(f'  digest: "{RUNTIME}"\n', values)
        self.assertIn(f"image: {PUBLIC}/appa-runtime@{RUNTIME}", render(chart, values))

    def test_demo_chart_pulls_the_pinned_digests(self):
        chart = ROOT / "integrations/kagent/demo/chart"
        values = DIGESTS.pin((chart / "values.yaml").read_text(),
                             {"appa-demo-tools": TOOLS, "appa-demo-mocks": MOCKS})
        output = render(chart, values)
        self.assertIn(f"image: {PUBLIC}/appa-demo-tools@{TOOLS}", output)
        self.assertIn(f"image: {PUBLIC}/appa-demo-mocks@{MOCKS}", output)

    def test_only_the_named_block_changes(self):
        values = (
            "mocks:\n  image:\n    repository: r/appa-demo-mocks\n    tag: \"\"\n    digest: \"\"\n"
            "tools:\n  image:\n    repository: r/appa-demo-tools\n    digest: \"\"\n"
        )
        pinned = DIGESTS.pin(values, {"appa-demo-tools": TOOLS})
        self.assertEqual(pinned, values.replace(
            "appa-demo-tools\n    digest: \"\"", f"appa-demo-tools\n    digest: \"{TOOLS}\""))

    def test_refuses_what_it_cannot_pin_exactly(self):
        for values, pins in (
            ("image:\n  repository: r/appa-runtime\n", {"appa-runtime": RUNTIME}),  # no digest line
            ("image:\n  repository: r/appa-runtime\n  digest: \"sha256:x\"\n", {"appa-runtime": RUNTIME}),
            ("a:\n  repository: r/appa-runtime\n  digest: \"\"\nb:\n  repository: r/appa-runtime\n  digest: \"\"\n",
             {"appa-runtime": RUNTIME}),
            ("image:\n  repository: r/appa-runtime\n  digest: \"\"\n", {"appa-other": RUNTIME}),
            ("image:\n  repository: r/appa-runtime\n  digest: \"\"\n", {"appa-runtime": "latest"}),
            # A digest key of a nested mapping is not this image's.
            ("image:\n  repository: r/appa-runtime\n  test:\n    digest: \"\"\n", {"appa-runtime": RUNTIME}),
        ):
            with self.subTest(values=values, pins=pins), self.assertRaises(ValueError):
                DIGESTS.pin(values, pins)


if __name__ == "__main__":
    unittest.main()
