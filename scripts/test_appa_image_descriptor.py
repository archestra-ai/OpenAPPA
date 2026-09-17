import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("appa-image-descriptor.py")
spec = importlib.util.spec_from_file_location("image_descriptor", SCRIPT)
descriptor = importlib.util.module_from_spec(spec)
spec.loader.exec_module(descriptor)
DIGEST = "sha256:" + "a" * 64


class ImageDescriptorTests(unittest.TestCase):
    def test_publication_records_runtime_and_python_without_demo_overwrite(self):
        repository = SCRIPT.resolve().parent.parent
        workflow = (repository / ".github/workflows/publish-oci-images.yml").read_text()
        step = workflow.split("      - name: Merge and tag image manifests\n", 1)[1]
        lines = step.split("        run: |\n", 1)[1].splitlines()
        body = []
        for line in lines:
            if line and not line.startswith("          "):
                break
            body.append(line[10:])
        # The production program runs unchanged except its scratch directories.
        # Only Docker is substituted: publishing to a real registry is not a unit test.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "docker"
            binary.write_text(f"#!{sys.executable}\n" + '''
import json
import sys
images = ["appa-runtime", "appa-kagent-adk", "appa-demo-tools", "appa-demo-mocks"]
if "create" in sys.argv:
    sys.exit(0)
reference = sys.argv[-1]
image = next(name for name in images if "/" + name + ":" in reference or "/" + name + "@" in reference)
digest = "sha256:" + "abcd"[images.index(image)] * 64
if "--format" in sys.argv:
    print(digest)
elif "--raw" in sys.argv:
    print(json.dumps({"manifests": [{"digest": digest, "platform": {"os": "linux", "architecture": arch}} for arch in ["amd64", "arm64"]]}))
''')
            binary.chmod(0o755)
            for image in ("appa-runtime", "appa-kagent-adk", "appa-demo-tools", "appa-demo-mocks"):
                inputs = root / "digests" / image
                inputs.mkdir(parents=True)
                for value in ("a", "b"):
                    (inputs / (value * 64)).touch()
            program = "\n".join(body).replace("/tmp/digests", str(root / "digests")).replace(
                "/tmp/appa-generation-images", str(root / "identities"))
            registry = subprocess.check_output([sys.executable, str(SCRIPT.with_name("appa-oci-tags.py")), "registry"], text=True).strip()
            environment = dict(os.environ, REGISTRY=registry, TAGS="v1.2.3", IMMUTABLE="false", PATH=str(root) + os.pathsep + os.environ["PATH"])
            # macOS /bin/bash is 3.x; the workflow uses GitHub's modern bash.
            bash = shutil.which("bash")
            result = subprocess.run([bash, "-euo", "pipefail", "-c", program], cwd=repository,
                                    env=environment, capture_output=True, text=True, timeout=15)
            self.assertEqual(result.returncode, 0, result.stderr)
            identities = {path.stem: json.loads(path.read_text()) for path in (root / "identities").iterdir()}
            self.assertEqual(set(identities), {"runtime", "python"})
            self.assertEqual(identities["runtime"]["digest"], "sha256:" + "a" * 64)
            self.assertEqual(identities["python"]["digest"], "sha256:" + "b" * 64)

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
