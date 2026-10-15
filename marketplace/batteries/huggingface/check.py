"""Validate an explicit token or, for a single-user deployment, the login cached by hf auth login."""
import json
import os
import urllib.error
import urllib.request
from hf_token import TOKEN_VAR, Deployment, env_token, resolve_token, hub_root


def check(environ=None, open_url=urllib.request.urlopen, deployment=Deployment.SINGLE_USER):
    environ = os.environ if environ is None else environ
    match deployment:
        case Deployment.SINGLE_USER:
            authentication = "token" if environ.get(TOKEN_VAR, "").strip() else "cli"
            token_of, missing = resolve_token, "cli_not_authenticated"
        case Deployment.MULTI_USER:
            authentication, token_of, missing = "token", env_token, "missing_credential"
    def result(status, reason):
        return {"status": status, "authentication": authentication, "reason": reason}
    try:
        token = token_of(environ)
    except RuntimeError:
        return result("needs_configuration", missing)
    try:
        request = urllib.request.Request(hub_root(environ) + "/api/whoami-v2", headers={"Authorization": f"Bearer {token}"})
        with open_url(request, timeout=8) as response:
            body = json.load(response)
            if not isinstance(body, dict) or not body.get("name"):
                return result("unavailable", "check_failed")
    except urllib.error.HTTPError as error:
        if error.code in (401, 403):
            return result("needs_configuration", "invalid_credential" if error.code == 401 else "insufficient_access")
        return result("unavailable", "provider_unavailable")
    except (OSError, ValueError):
        return result("unavailable", "provider_unavailable")
    return result("ready", "verified")


if __name__ == "__main__":
    print(json.dumps(check()))
