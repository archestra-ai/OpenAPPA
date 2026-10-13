"""The linear audience source of a multi-user deployment: one consult in,
one answer out.

The token is an OAuth app's access token issued with `actor=app`: no
person stands behind it, so there is no viewer, and a personal API key,
which acts as its owner, is refused. Serves audience-source.py's
templates other than `viewer`, and its member lookup; a member is still
the account's email as Linear reports it.
"""

import importlib.util
from pathlib import Path

SPEC = importlib.util.spec_from_file_location("audience_source", Path(__file__).with_name("audience-source.py"))
linear = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(linear)


SERVED_TEMPLATES = [
    "full-members",
    "team/<key>/members",
    "team/<key>/readers",
    "issue/<id>/readers",
    "project/<id>/readers",
    "document/<id>/readers",
]


def answer(call, artifact):
    match artifact:
        case {"selector": "viewer"}:
            raise ValueError("a multi-user deployment has no viewer")
        case _:
            return linear.answer(call, artifact)


def app_token(token):
    if token.startswith("lin_api_"):
        raise ValueError("a personal API key acts as its owner; set an OAuth app's access token")
    return f"Bearer {token}"


if __name__ == "__main__":
    linear.serve(SERVED_TEMPLATES, answer, app_token)
