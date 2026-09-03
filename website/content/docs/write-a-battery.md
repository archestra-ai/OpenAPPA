---
title: Create your own battery
category: Batteries
order: 6.7
description: Create, test, and submit a battery for your MCP server.
---

This guide shows MCP server authors how to create and publish a battery for their server.

> **Ask your coding agent**
>
> Give your coding agent a link to this page and use this prompt:
>
> ```text
> Read the OpenAPPA battery guide I linked.
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

Your coding agent reads the server code and documentation. It finds every tool, what arguments it accepts, what it returns, and what it can change.

The agent then:

- creates the battery config and any annotator or audience source scripts;
- records the exact server version, the tools it covers, and an explanation of each rule;
- adds the battery documentation page and catalogue card;
- runs the tests below; and
- lists anything that the server code does not make clear.

Before you submit the pull request, check:

- The battery covers every tool in the server version it names.
- The battery identifies every tool that sends data or changes something.
- Each tool sends data only to the people or services you expect.
- Each result's `audience` allows only the people who should see it.
- Results from sources you have not vetted are marked `suspicious`, not `trusted`.
- The battery asks a person before every action that needs approval.
- The agent has clearly listed every question it could not answer.

The agent should link each rule to the server code or documentation that supports it. Follow those links to check the list above.

## Add the battery files

Create a folder under `batteries/`:

```text
batteries/
`-- your-server/
    |-- README.md
    |-- appa.toml
    |-- annotator.py
    `-- test_annotator.py
```

`appa.toml` contains the tool contracts. Add an annotator when a contract depends on the call's arguments. Add an audience source when the service's users or groups define who can receive data.

The battery `README.md` must name the server version and list the covered tools. It must also explain each contract, script, test, and known limit.

Add the battery config to `include` in `examples/claude-code-battery/appa.toml`. Add the Authority and audience source settings it needs.

The test suite loads this example to make sure all included batteries work together.

## Test the battery

Write tests for every annotator and audience source. Use saved API responses instead of calling the real service.

Test expected input, invalid input, provider errors, and missing data.

Run the battery's Python tests with:

```sh
python3 -m unittest discover -s batteries/your-server -p 'test_*.py'
```

Run the project tests:

```sh
cargo test -p appa --test examples_load
cargo test --workspace --locked
```

The first command loads `examples/claude-code-battery/appa.toml` with all its batteries. CI also runs Python tests under `batteries/*/test_*.py`.

## Open the pull request

Open a pull request against `archestra-ai/OpenAPPA` `main`. Include:

- the new battery folder;
- its `README.md`, scripts, and tests;
- the updated `include` list in `examples/claude-code-battery/appa.toml`;
- a battery documentation page under `website/content/docs/`;
- a card in `website/components/BatteryCatalog.tsx`; and
- the server repository, exact version or commit, covered tools, and test results in the pull request description.

## Verify the pull request

The pull request is ready for review when:

1. Repository CI passes.
2. Every contract uses the exact tool name and arguments from that server version.
3. Every script passes its tests and refuses invalid input.
4. The test that loads all batteries passes.
5. The catalogue card opens the battery documentation page.
