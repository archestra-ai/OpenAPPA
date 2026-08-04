# OpenAPPA evaluates the complete Tau Knowledge split

This package produces a custom OpenAPPA result for the `banking_knowledge` domain on the [Tau leaderboard](https://taubench.com/leaderboard?benchmark=knowledge). Tau executes every authorized tool call and applies its stock evaluator, while OpenAPPA checks each proposed call and receives the real result before the agent continues. The runner pins Tau 1.0.1 at commit `93ee97b8303ce0e89e0ad17e6207591a1846f84b`, uses the complete `base` task split, and refuses results with fewer than four trials or any infrastructure-error simulation.

## The setup pins code, data, and Knowledge dependencies

Tau keeps benchmark data outside its Python package, so the setup script clones the matching repository revision into `.tau2-bench/` and installs the `knowledge` dependency extra. The AllTools retrieval configurations also require Anthropic's sandbox runtime and platform executables. Install them before running the setup script so it can verify the complete local surface.

```sh
npm install -g @anthropic-ai/sandbox-runtime@0.0.23

# macOS
brew install ripgrep

# Ubuntu or Debian
sudo apt-get install ripgrep bubblewrap socat

./setup-taubench.sh
```

`alltools-qwen` is the default because the default agent model already uses OpenRouter, while `alltools` uses OpenAI embeddings. Both configurations expose BM25 search, dense search, and a read-only shell, and both are accepted by the leaderboard. Set the corresponding key and run the no-cost preflight before starting an evaluation.

```sh
export OPENROUTER_API_KEY=...
uv run appa-taubench preflight

# For OpenAI embeddings instead:
export OPENAI_API_KEY=...
uv run appa-taubench preflight --retrieval-config alltools
```

The preflight checks the pinned checkout, Knowledge Python dependencies, API-key names, sandbox executables, policy coverage, and the current base task set. It does not construct an embedding index, invoke a retrieval API, or call either conversation model. A successful preflight therefore establishes readiness without spending model or embedding credits.

## A run is submission-shaped by default

The run command evaluates every current `banking_knowledge` base task with four trials, Tau's 200-step limit, seed 300, and concurrency three. It has no task filter or stock-agent arm, so the default invocation currently schedules 388 OpenAPPA simulations. Specify exact provider model identifiers before buying the run because model aliases can change outside this repository.

```sh
# Validate and print the exact 388-simulation plan without invoking Tau.
uv run appa-taubench run --dry-run \
  --model openrouter/openai/gpt-4.1-mini \
  --user-model openrouter/openai/gpt-4.1-mini

# Start the paid evaluation only after reviewing the plan.
uv run appa-taubench run \
  --model openrouter/openai/gpt-4.1-mini \
  --user-model openrouter/openai/gpt-4.1-mini
```

Each output directory name contains a digest of every run setting, the rendered policy, the harness implementation, the OpenAPPA binding identity, and the Tau revision. The same information is written to `run-config.json`, and resume is accepted only when that manifest matches exactly. Changing the model, user model, retrieval configuration, trials, seed, concurrency, limits, policy, or harness source selects a different output directory instead of mixing trajectories.

Tau's `results.json` remains the scored trajectory source. The adjacent `appa-audit/` directory retains the raw model completions, hidden policy retries, logical policy calls, rewritten Tau dispatches, original tool results, and delivery dispositions that Tau's standard trajectory cannot represent. Tool-call IDs and task IDs correlate the two records for review.

## Discoverable operations keep their logical policy identity

Tau exposes specialized banking operations through `call_discoverable_agent_tool`, whose `agent_tool_name` argument may select either a read or a mutation. The harness registers those hidden operations as logical OpenAPPA tools, checks the inner name and decoded arguments, and wraps an authorized logical dispatch back into Tau's stock dispatcher. Tau still performs the underlying operation, records its database effects, and scores the same executed call.

The committed contract treats bank-authored retrieval results and dispatcher metadata as neutral, while customer and account readers contribute `suspicious` trust. Mutations and transfers require `internal` trust, so a trajectory that has admitted customer-specific data cannot subsequently commit those effects without a remedy supplied by the registered policy. This conservative contract can reduce benchmark utility, and that consequence belongs in the published custom methodology.

## Submission preparation enforces custom disclosure

After a complete run, the submit command invokes Tau's public trajectory verification and interactive submission preparation, adds the APPA audit artifacts, forces honest custom-scaffold metadata, and runs Tau's final submission validator. Answer the contact, organization, model, and evaluation-date prompts with publication-ready values. The wrapper sets `submission_type` to `custom`, `modified_prompts` to `true`, `omitted_questions` to `false`, adds the OpenAPPA implementation reference, and records the mediation and trajectory-rewriting details.

```sh
uv run appa-taubench submit runs/EXACT_RUN_DIRECTORY --output prepared-submission
```

Upload the complete prepared directory, including `trajectories/`, `appa-audit/`, and `run-config.json`, to external storage for maintainer review. Copy only `submission.json` into a new directory under `web/leaderboard/public/submissions/` in the Tau repository, add that directory to the text `submissions` array in `manifest.json`, and include the external artifact link in the pull request. The authoritative field and pull-request requirements remain in Tau's [leaderboard submission guide](https://github.com/sierra-research/tau2-bench/blob/main/docs/leaderboard-submission.md).
