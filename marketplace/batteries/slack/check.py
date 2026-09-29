"""Verify Slack authentication without emitting identity or credential data."""
import json
import os
import urllib.error
from slack_api import TOKEN_VAR, web_api


def check(environ=None, api=web_api):
    environ = os.environ if environ is None else environ
    token = environ.get(TOKEN_VAR, "").strip()
    def result(status, reason):
        return {"status": status, "authentication": "token", "reason": reason}
    if not token:
        return result("needs_configuration", "missing_credential")
    try:
        body = api(token)("auth.test")
        if not isinstance(body, dict):
            return result("unavailable", "check_failed")
        if body.get("ok") is True:
            return result("ready", "verified")
        if body.get("error") in {"invalid_auth", "not_authed", "token_revoked", "account_inactive"}:
            return result("needs_configuration", "invalid_credential")
        if body.get("error") == "missing_scope":
            return result("needs_configuration", "insufficient_access")
        return result("unavailable", "provider_unavailable")
    except (OSError, ValueError):
        return result("unavailable", "provider_unavailable")


if __name__ == "__main__":
    print(json.dumps(check()))
