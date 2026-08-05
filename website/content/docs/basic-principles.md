---
title: "Basic Principles"
category: Operations
order: 7
description: Running OpenAPPA in an organization — stacked configuration, cross-platform enforcement, and change management.
---

One of OpenAPPA's core goals is to work in practice, not only in theory. That means offering a concrete way to apply OpenAPPA inside an organization: how to integrate it into business processes, how to govern it, and how to manage change over time.

## A cross-platform language for agent security

OpenAPPA is cross-platform. A single configuration ([the policy reference](/docs/contracts) describes the format) can be applied at different levels, on different stacks, and by different roles.

:::fig-policy-stack:::

Not every policy and mechanism can be applied at every level, of course. LLM proxies, for example, are limited in how much of an agent's trajectory they can steer. To keep enforcement guaranteed, a validator checks a configuration against the deployment options actually available.

## Configuration stacking

An OpenAPPA configuration is a `.toml` file designed for stacking, and we suggest mirroring the structure of the company with the structure of the files. A base configuration can define the core audiences and other primitives, along with how OpenAPPA behaves under uncertainty. Further configurations, owned by departments and teams, then refine it — extending what agents can do without weakening the security requirements set centrally.


## Ready for change at scale

We provide best practices for change management over large configurations. At their core is the same validator, run in CI/CD, verifying every change to the OpenAPPA configuration in the shared repository.
