"""The readiness check of a multi-user deployment: check.py's check of
the deployment's token alone, never a CLI login found on the host."""

import json

import check
from hf_token import Deployment

if __name__ == "__main__":
    print(json.dumps(check.check(deployment=Deployment.MULTI_USER)))
