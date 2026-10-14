"""The linear audience source of a multi-user deployment: one consult in,
one answer out.

The token is an OAuth app's access token issued with `actor=app`: no
person stands behind it, so there is no viewer, and a personal API key,
which acts as its owner, is refused. Serves audience-source.py's
templates other than `viewer`, and the member lookup (see
linear_audience.py); a member is still the account's email as Linear
reports it.
"""

import linear_audience


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
            return linear_audience.answer(call, artifact)


def app_token(token):
    if token.startswith("lin_api_"):
        raise ValueError("a personal API key acts as its owner; set an OAuth app's access token")
    return f"Bearer {token}"


if __name__ == "__main__":
    linear_audience.serve(SERVED_TEMPLATES, answer, app_token)
