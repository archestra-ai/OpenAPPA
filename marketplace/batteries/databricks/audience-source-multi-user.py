"""The databricks audience source of a multi-user deployment: one consult in,
one answer out.

The single-user source's selectors and member lookup without `viewer`: the
login is a service principal's, with no person behind it. Readers are spelled
as databricks_audience.py spells them, a user by the address they sign in
with. Every CLI command runs as the service principal the binding's
APPA_PROVIDER_DATABRICKS_HOST, _CLIENT_ID, and _CLIENT_SECRET name, and as
nobody else (see service_principal.py).
"""

import os

import databricks_audience
import service_principal

SERVED_TEMPLATES = ["members", "group/<name>", "genie-space/<id>/readers"]


def answer(run, artifact):
    match artifact:
        case {"selector": "viewer"}:
            raise ValueError("a service principal is no viewer; this source serves no `viewer`")
        case _:
            return databricks_audience.answer(run, artifact)


if __name__ == "__main__":
    databricks_audience.serve(SERVED_TEMPLATES, answer, lambda: service_principal.environment(os.environ))
