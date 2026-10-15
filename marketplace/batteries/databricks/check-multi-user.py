"""The readiness check of a multi-user deployment: check.py's check of the
service principal's login (see check.check_service_principal)."""

import json

import check

if __name__ == "__main__":
    print(json.dumps(check.check_service_principal()))
