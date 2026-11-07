"""The request and the attestation subjects name the same artifacts."""

import hashlib
import importlib.util
import tempfile
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location("subjects", Path(__file__).with_name("appa-release-subjects.py"))
SUBJECTS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SUBJECTS)

GO = "sha256:" + "9" * 64


def digests(**overrides):
    value = {image: "sha256:" + str(index) * 64 for index, image in enumerate(SUBJECTS.IMAGES)}
    value["appa-kagent-adk-go"] = value["golang-adk"] = GO
    value.update(overrides)
    return value


class ReleaseSubjectTests(unittest.TestCase):
    def charts(self, version="0.39.0", extra=()):
        directory = Path(tempfile.mkdtemp())
        for name in (*(f"{chart}-{version}.tgz" for chart in SUBJECTS.CHARTS), *extra):
            (directory / name).write_bytes(name.encode())
        return directory

    def test_every_image_under_its_public_name_and_charts_by_file(self):
        images, lines = SUBJECTS.subjects("0.39.0", digests(), self.charts())
        public = SUBJECTS.PUBLIC
        self.assertEqual(set(images), {f"{public}/{image}" for image in SUBJECTS.IMAGES})
        self.assertEqual(images[f"{public}/golang-adk"], GO)
        for name, digest in images.items():
            self.assertIn(f"{digest[7:]}  {name}", lines)
        chart = "appa-runtime-0.39.0.tgz"
        self.assertIn(f"{hashlib.sha256(chart.encode()).hexdigest()}  {chart}", lines)
        self.assertEqual(len(lines), len(SUBJECTS.IMAGES) + len(SUBJECTS.CHARTS))

    def test_refuses_an_incomplete_or_inconsistent_release(self):
        missing = digests()
        del missing["appa-demo-mocks"]
        for version, value, charts in (
            ("0.39.0", missing, self.charts()),
            ("0.39.0", digests(**{"appa-runtime": ""}), self.charts()),
            ("0.39.0", digests(**{"golang-adk": "sha256:" + "8" * 64}), self.charts()),
            ("0.39.0", {**digests(), "other": GO}, self.charts()),
            ("0.39.0", digests(), self.charts(version="0.38.0")),
            ("0.39.0", digests(), self.charts(extra=("appa-other-0.39.0.tgz",))),
            ("v0.39.0", digests(), self.charts()),
        ):
            with self.subTest(version=version, value=value, charts=sorted(p.name for p in charts.iterdir())):
                with self.assertRaises(ValueError):
                    SUBJECTS.subjects(version, value, charts)


if __name__ == "__main__":
    unittest.main()
