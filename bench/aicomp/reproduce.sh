#!/bin/sh
# Replays both workloads on every model and every defense, then prints the tables.
# Needs OPENROUTER_API_KEY in the environment or in .env next to this script.
# OUT picks the output directory, so repeated replays can sit side by side.
set -eu
cd "$(dirname "$0")"

ARMS=none,rules,optimal,sticky,sticky-intent,appa-q
WORKERS=${WORKERS:-12}
OUT=${OUT:-runs}
MODELS="openai/gpt-oss-20b=gptoss20b google/gemma-4-26b-a4b-it=gemma4-26b z-ai/glm-5.3-flash=glm53flash openai/gpt-6-luna=gpt6luna"

mkdir -p "$OUT"
pids=""
for entry in $MODELS; do
  model=${entry%%=*}
  tag=${entry#*=}
  uv run appa-aicomp run --model "$model" --sets corpus,washout --arms "$ARMS" --workers "$WORKERS" --out "$OUT/corpus-$tag" > "$OUT/corpus-$tag.log" 2>&1 &
  pids="$pids $!"
  uv run appa-aicomp run --model "$model" --sets triage-all --arms "$ARMS" --workers "$WORKERS" --out "$OUT/triage-$tag" > "$OUT/triage-$tag.log" 2>&1 &
  pids="$pids $!"
done

failed=0
for pid in $pids; do
  wait "$pid" || failed=1
done
if [ "$failed" -ne 0 ]; then
  echo "a replay failed; see $OUT/*.log" >&2
  exit 1
fi

uv run python -m appa_aicomp.headline --corpus "$OUT"/corpus-*/ --triage "$OUT"/triage-*/
