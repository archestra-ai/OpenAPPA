"""The linear audience source of a single-user deployment, over one
person's personal API key, sent as is (see linear_audience.py). The
runtime runs this script once for each membership read or member lookup,
passes the consult request as JSON on standard input, and reads one JSON
answer from standard output."""

from linear_audience import answer, serve

SERVED_TEMPLATES = [
    "viewer",
    "full-members",
    "team/<key>/members",
    "team/<key>/readers",
    "issue/<id>/readers",
    "project/<id>/readers",
    "document/<id>/readers",
]

if __name__ == "__main__":
    serve(SERVED_TEMPLATES, answer, lambda token: token)
