"""The `github` context provider of a multi-user deployment: context.py's
consult and answer, with every person named `github:<id>` and no viewer
(see context.py)."""

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import context  # noqa: E402

if __name__ == "__main__":
    context.main(context.Deployment.MULTI_USER)
