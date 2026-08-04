# OpenAPPA TauBench evaluation

This package evaluates OpenAPPA through TauBench's public custom-agent interface. The stock TauBench environment still executes every allowed tool call and the stock evaluator scores the resulting trajectory. OpenAPPA checks a call before execution and receives the real TauBench result afterward, so neither project needs a benchmark-specific execution path.

The initial evaluation covers the Airline domain with a trust-only contract. Customer-record readers add `suspicious` trust, while mutations and egress require `internal` trust; airline-owned reference data contributes the fold identity. The policy uses only tool source types and never reads task instructions, expected actions, or other benchmark ground truth.

```sh
./setup-taubench.sh
export OPENROUTER_API_KEY=...

uv run appa-taubench
```

TauBench keeps benchmark data outside its Python package. The setup script clones its repository at the same revision as the pinned Python dependency into the ignored `.tau2-bench/` directory, then syncs the harness environment. The CLI uses that checkout by default; pass `--tau2-data-dir PATH` or set `TAU2_DATA_DIR` to use another checkout. This keeps the benchmark data and its implementation unmodified.

The default smoke evaluation compares the stock agent and OpenAPPA on tasks 27 and 40 with `openrouter/openai/gpt-4.1-mini`, one trial each. Task 27 is read-only, while task 40 reads a reservation and then proposes a mutation that the contract refuses after customer-authored data enters the trajectory. Results and exact trajectories are written below `runs/`; rerunning the same configuration resumes the saved run rather than buying the same completions again.

Use `--defense appa` to run only OpenAPPA, `--defense none` for only the stock agent, or pass different task IDs and models explicitly. The harness permits one sequential tool call per model completion because the embedded `CallSession` has one in-flight dispatch slot. Hidden policy-retry completions are included in the reported agent cost, and the summary reports checks, blocks, admissions, and sealed results.

This is a utility and integration smoke evaluation rather than a security benchmark. Standard TauBench contains no indirect prompt-injection attack set, and two tasks cannot support a comparative quality claim. The result establishes that OpenAPPA participates in live TauBench trajectories and shows the utility consequence of this specific conservative contract.
