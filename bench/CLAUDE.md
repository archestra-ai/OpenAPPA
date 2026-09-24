# Benchmark result sources

Published benchmark results live in the private GCS bucket
`gs://archestra-appa-bench-archive`. Treat the bucket as the source of truth.
Each `bench/*/results/*/archive-index.json` indexes one published run, and the
adjacent `summary.json` is its machine-readable result.

README tables, `website/content/docs/evaluation.md`, and the historical paper
are derived or historical artifacts. Never use them as result sources.

Authenticated contributors can inspect and retrieve the archive with:

```sh
gcloud storage ls -r 'gs://archestra-appa-bench-archive/**'
gcloud storage cat 'gs://archestra-appa-bench-archive/bench/<benchmark>/<commit>/<run>/index.json'
gcloud storage cp 'gs://archestra-appa-bench-archive/bench/<benchmark>/<commit>/<run>/<archive>' .
sha256sum --check <(jq -r '"\(.sha256)  \(.archive)"' archive-index.json)
tar --zstd -tf '<archive>'
```
