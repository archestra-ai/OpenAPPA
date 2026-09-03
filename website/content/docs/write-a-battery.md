---
title: Create your own battery
category: Batteries
order: 6.7
description: Create, test, and submit a battery for your MCP server.
---

This guide shows MCP server authors how to create and publish a battery for their server.

> **Ask your coding agent**
>
> Copy this prompt into your coding agent:
>
> ```text
> Read https://openappa.com/write-a-battery and follow the guide.
>
> Inspect the MCP server in this repository.
> Create a battery for it.
> Add its documentation and catalogue card.
> Run every available check.
> Do not guess when the server code does not answer a question.
> Tell me what you created and what you verified.
> Tell me what I still need to review.
> ```

## What happens next

1. **Read.** Your agent reads the server code and docs. It lists every tool, its inputs and results, and anything it can change.
2. **Build.** It creates the battery, tests, documentation page, and catalogue card. It records the exact server version and links each rule to the source that supports it.
3. **Review.** You open those links, check the rules, resolve any unanswered questions, and submit the pull request.

:::battery-review-checklist:::

## Add the battery files

Create one folder under `batteries/`:

```text
batteries/
`-- your-server/
    |-- README.md
    |-- appa.toml
    |-- annotator.py         # optional
    |-- audience-source.py   # optional
    `-- test_*.py            # required for scripts
```

Only `README.md` and `appa.toml` are always required.

- **`appa.toml` — tool rules.** OpenAPPA calls each rule a tool contract. It labels the result, sets conditions for the call, and records what the tool sends or changes.
- **`README.md` — what the battery covers.** Include the server repository and exact version, the covered tools, why each rule exists, and any known limits.
- **`annotator.py` — an optional decision script.** Add one when a rule changes based on an argument, such as a path, channel, or recipient.
- **`audience-source.py` — an optional people-and-group lookup.** Add one when the service decides who can receive data.
- **`test_*.py` — offline tests.** Test every script in the battery.

Update `examples/claude-code-battery/appa.toml`:

- Add the new `appa.toml` to its `include` list.
- If an action needs human approval, configure an Authority, OpenAPPA's approval component.
- Configure any audience source the battery uses.

The test suite loads the example to check that the batteries work together.

## Test the battery

Write tests for every script. Use saved API responses instead of calling the real service.

Test normal input, invalid input, service errors, and missing data.

Run the battery's Python tests with:

```sh
python3 -m unittest discover -s batteries/your-server -p 'test_*.py'
```

Run the project tests:

```sh
cargo test -p appa --test examples_load
cargo test --workspace --locked
```

The first command checks that the example and its batteries load together. CI also runs every `batteries/*/test_*.py` file.

## Open the pull request

1. Push your branch to GitHub. Use a fork if you do not have write access.
2. Open a pull request to the `main` branch of `archestra-ai/OpenAPPA`.

Check that the pull request includes:

- the new battery folder, including its `README.md`, scripts, and tests;
- the battery in the `include` list of `examples/claude-code-battery/appa.toml`;
- a page under `website/content/docs/` and a card in `website/components/BatteryCatalog.tsx`; and
- a description with the server repository, exact version or commit, covered tools, and test results.

## Verify the pull request

Before you submit, check that:

1. Every contract uses the exact tool name and arguments from the named server version.
2. Every script passes its tests and rejects invalid input.
3. The test that loads all batteries passes.
4. The catalogue card opens the battery documentation page.
5. Repository CI passes.
