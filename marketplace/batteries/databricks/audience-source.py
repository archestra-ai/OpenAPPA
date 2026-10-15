"""The databricks audience source of a single-user deployment: one consult
in, one answer out, over the person's own CLI login (see
databricks_audience.py)."""

import os

from databricks_audience import answer, serve

SERVED_TEMPLATES = ["viewer", "members", "group/<name>", "genie-space/<id>/readers"]

if __name__ == "__main__":
    serve(SERVED_TEMPLATES, answer, lambda: os.environ)
