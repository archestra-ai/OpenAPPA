"""The github repository annotators of a multi-user deployment:
repository-visibility.py's answers, under the deployment's token alone,
never a CLI login found on the host (see github_token.env_token)."""

import importlib.util
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from github_token import env_token  # noqa: E402

SPEC = importlib.util.spec_from_file_location("repository_visibility", os.path.join(HERE, "repository-visibility.py"))
ANNOTATOR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ANNOTATOR)

if __name__ == "__main__":
    ANNOTATOR.main(env_token)
