#!/bin/sh
# Replays both workloads on every model and every defense, then prints the tables.
# Needs OPENROUTER_API_KEY in the environment or in .env next to this script.
# REPLAYS replays run one after another into $OUT/r1, $OUT/r2, ...; the tables pool them.
set -eu
cd "$(dirname "$0")"

ARMS=none,rules,optimal,sticky,sticky-intent,appa-q
MAX_CONCURRENCY=${MAX_CONCURRENCY:-16}
OUT=${OUT:-runs}
REPLAYS=${REPLAYS:-3}
MODELS="openai/gpt-oss-20b=gptoss20b google/gemma-4-26b-a4b-it=gemma4-26b z-ai/glm-5.3-flash=glm53flash openai/gpt-6-luna=gpt6luna"

replay() {
  dir=$1
  mkdir -p "$dir"
  pids=""
  for entry in $MODELS; do
    model=${entry%%=*}
    tag=${entry#*=}
    uv run appa-aicomp run --model "$model" --sets corpus,washout --arms "$ARMS" --max-concurrency "$MAX_CONCURRENCY" --out "$dir/corpus-$tag" > "$dir/corpus-$tag.log" 2>&1 &
    pids="$pids $!"
    uv run appa-aicomp run --model "$model" --sets triage-all --arms "$ARMS" --max-concurrency "$MAX_CONCURRENCY" --out "$dir/triage-$tag" > "$dir/triage-$tag.log" 2>&1 &
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
  i=$((i + 1))
done

uv run python -m appa_aicomp.headline --corpus "$OUT"/r*/corpus-*/ --triage "$OUT"/r*/triage-*/
