#!/bin/sh
# Replays both workloads on every model and every defense, then prints the tables.
# Needs OPENROUTER_API_KEY in the environment or in .env next to this script.
set -eu
cd "$(dirname "$0")"

ARMS=none,rules,optimal,sticky,sticky-intent,appa-q
WORKERS=${WORKERS:-16}

replay() {
  uv run appa-aicomp run --model "$1" --sets corpus,washout --arms "$ARMS" --workers "$WORKERS" --out "runs/corpus-$2" > "runs/corpus-$2.log" 2>&1 &
  uv run appa-aicomp run --model "$1" --sets triage-all --arms "$ARMS" --workers "$WORKERS" --out "runs/triage-$2" > "runs/triage-$2.log" 2>&1 &
}

mkdir -p runs
replay openai/gpt-oss-20b gptoss20b
replay google/gemma-4-26b-a4b-it gemma4-26b
replay z-ai/glm-5.3-flash glm53flash
replay openai/gpt-6-luna gpt6luna
wait

uv run python -m appa_aicomp.headline --corpus runs/corpus-* --triage runs/triage-*
