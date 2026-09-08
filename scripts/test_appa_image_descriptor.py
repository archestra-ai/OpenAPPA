import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import unittest

SCRIPT = Path(__file__).with_name("appa-image-descriptor.py")
spec = importlib.util.spec_from_file_location("image_descriptor", SCRIPT)
descriptor = importlib.util.module_from_spec(spec)
spec.loader.exec_module(descriptor)
DIGEST = "sha256:" + "a" * 64


class ImageDescriptorTests(unittest.TestCase):
    def test_index_records_platforms_and_skips_attestations(self):
        manifest = {"manifests": [
            {"digest": DIGEST, "platform": {"os": "linux", "architecture": "amd64"}},
            {"digest": DIGEST, "platform": {"os": "linux", "architecture": "arm64"}},
            {"digest": DIGEST, "platform": {"os": "unknown", "architecture": "unknown"}},
        ]}
        self.assertEqual(descriptor.describe(DIGEST, manifest), {
            "digest": DIGEST, "platforms": {"linux/amd64": DIGEST, "linux/arm64": DIGEST}})
        manifest["manifests"].append(manifest["manifests"][0])
        with self.assertRaises(ValueError):
            descriptor.describe(DIGEST, manifest)

    def test_single_manifest_requires_the_build_platform(self):
        manifest = {"mediaType": "application/vnd.oci.image.manifest.v1+json"}
        with self.assertRaises(ValueError):
            descriptor.describe(DIGEST, manifest)
        self.assertEqual(descriptor.describe(DIGEST, manifest, "linux/amd64")["platforms"], {"linux/amd64": DIGEST})

    def test_subprocess_streams_and_error_status(self):
        for raw, success in [(b'{"manifests": []}', False),
                             (b'x' * (descriptor.MAX_BYTES + 1), False),
                             (b'{"mediaType":"application/vnd.oci.image.manifest.v1+json"}', True)]:
            result = subprocess.run([sys.executable, str(SCRIPT), DIGEST, "--single-platform", "linux/amd64"],
                                    input=raw, capture_output=True, timeout=10)
            self.assertEqual(result.returncode == 0, success)
            if success:
                self.assertEqual(json.loads(result.stdout)["digest"], DIGEST)
                self.assertEqual(result.stderr, b"")
            else:
                self.assertEqual(result.stdout, b"")
                self.assertTrue(result.stderr)


if __name__ == "__main__":
    unittest.main()
