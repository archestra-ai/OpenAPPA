"""The github repository annotators of a multi-user deployment:
repository_visibility.py's answers, under the deployment's token alone,
never a CLI login found on the host (see github_token.env_token)."""

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from github_token import env_token  # noqa: E402
import repository_visibility  # noqa: E402

if __name__ == "__main__":
    repository_visibility.main(env_token)
