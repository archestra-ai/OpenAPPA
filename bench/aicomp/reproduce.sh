#!/bin/sh
# Pulls the published solutions, extracts their attack candidates, replays them with the benign tasks
# on both models and all four arms REPLAYS times, reruns rows that failed on a provider 429, then
# prints the tables. Needs OPENROUTER_API_KEY in the environment or in .env next to this script.
set -eu
cd "$(dirname "$0")"

OUT=${OUT:-runs}
REPLAYS=${REPLAYS:-3}
MODELS="openai/gpt-oss-20b=parasail/fp4=gpt-oss-20b google/gemma-4-26b-a4b-it=nextbit/bf16=gemma-4-26b"

uv run appa-aicomp-candidates fetch --into "$OUT/sources"
uv run appa-aicomp-candidates extract --sources "$OUT/sources" --out "$OUT/candidates"

replay() {
  dir=$1
  shift
  pids=""
  for entry in $MODELS; do
    model=${entry%%=*}
    rest=${entry#*=}
    provider=${rest%%=*}
    tag=${rest#*=}
    mkdir -p "$dir/$tag"
    uv run appa-aicomp --model "$model" --provider "$provider" \
      --candidates "$OUT/candidates/$(echo "$model" | tr / _).jsonl" --attacks 2000 --triage 319 --seed 20261002 \
      --out "$dir/$tag" "$@" > "$dir/$tag.log" 2>&1 &
    pids="$pids $!"
  done
  failed=0
  for pid in $pids; do
    wait "$pid" || failed=1
  done
  if [ "$failed" -ne 0 ]; then
    echo "a replay failed; see $dir/*.log" >&2
    exit 1
  fi
}

i=1
while [ "$i" -le "$REPLAYS" ]; do
  replay "$OUT/r$i"
  replay "$OUT/r$i" --max-concurrency 4 --retry-errors "Error code: 429"
  i=$((i + 1))
done

uv run appa-aicomp-summary "$OUT"
