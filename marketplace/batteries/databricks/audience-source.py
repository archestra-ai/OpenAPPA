"""The databricks audience source of a single-user deployment, over the
person's own CLI login (see databricks_audience.py). The runtime runs
this script once for each membership read or member lookup, passes the
consult request as JSON on standard input, and reads one JSON answer
from standard output."""

import os

from databricks_audience import answer, serve

SERVED_TEMPLATES = ["viewer", "members", "group/<name>", "genie-space/<id>/readers"]

if __name__ == "__main__":
    serve(SERVED_TEMPLATES, answer, lambda: os.environ)
