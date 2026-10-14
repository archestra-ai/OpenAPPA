"""The huggingface repository annotators of a multi-user deployment:
repo-visibility.py's answers under a shared token with no viewer, read
from the deployment alone, never a CLI login found on the host."""

import importlib.util
from pathlib import Path

SCRIPT = Path(__file__).resolve().with_name("repo-visibility.py")
SPEC = importlib.util.spec_from_file_location("repo_visibility", SCRIPT)
ANNOTATOR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ANNOTATOR)

if __name__ == "__main__":
    ANNOTATOR.run(ANNOTATOR.Deployment.MULTI_USER)
