"""The readiness check of a multi-user deployment: check.py's check of a
GitHub App installation's token (see check.check_installation)."""

import json

import check

if __name__ == "__main__":
    print(json.dumps(check.check_installation()))
