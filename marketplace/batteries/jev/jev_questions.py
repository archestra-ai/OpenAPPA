"""What the jev annotator asks about one tool call, and the worked examples it shows.

Four questions cover the two label dimensions of a tool contract: what the
call's result contributes (`delta.audience`, `delta.trust`) and what the
trajectory must satisfy before the call runs (`requires.audience`,
`requires.trust`). Effects and attention marks are not asked.

The criteria order is part of the prompt: reordering it moves answers.
"""

import json

FRAME = (
    "OpenAPPA is an information-flow policy engine that sits between an agent and "
    "its tools. Before each tool call it evaluates the call against the trajectory's "
    "security label, a pair of `audience` (who is allowed to read the data the agent "
    "holds) and `trust` (how much that data can be relied on). The trajectory starts "
    "at `{public, trusted}` and can only become more restrictive.\n\n"
    "You act as an APPA Annotator. For the tool call given in `tool` and `arguments`, "
    "supply the parts of its contract that concern audience and trust. Judge the "
    "specific arguments of this call, not the tool's general capability: the same "
    "tool is `public` in one call and `self` in another. A tool name that appears "
    "inside an argument value is not a destination this call reaches.\n\n"
    "Some calls address only the agent's own harness: loading a tool schema, invoking "
    "a skill, waiting on or reading a background task, entering a worktree, or running "
    "an OpenAPPA remedy plan by its offer id. Such a call delivers nothing to any "
    "reader outside this machine, and the harness itself produces its result."
)

DELTA_AUDIENCE_CRITERIA = {
    "public": (
        "The returned bytes are already readable today by someone outside the "
        "operator's organization: a fetched public web page, published "
        "documentation, or a file on a branch that is already pushed to a public "
        "remote. An acknowledgement, id, URL or status with no data in it is also "
        "`public`."
    ),
    "internal": (
        "The returned bytes are readable inside the operator's organization but not "
        "outside it: a local checkout, a working tree, an unpushed branch, a build "
        "or test run over them, a diff of uncommitted work, a private repository's "
        "code or history, an internal chat message, a customer or business record, "
        "an internal service's response, or a private document. This holds whatever "
        "the upstream repository's visibility is."
    ),
    "self": (
        "The returned bytes are restricted to the single identity OpenAPPA acts for "
        "and no one else: credentials, tokens, private keys, a shell history or "
        "environment dump, a local secret file, or personal mail — anything the "
        "operator would not show even to a colleague."
    ),
}

DELTA_AUDIENCE_INSTRUCTIONS = (
    f"{FRAME}\n\n"
    "Decide `delta.audience`, in two steps.\n\n"
    "First decide whether the call's result contains data at all. If the call "
    "returns only an acknowledgement, an id, a URL or a status, answer `public` and "
    "ignore everything the call sent. Argument values are never part of the result: "
    "a file's contents, a message body or a commit message travelling outward is not "
    "what comes back.\n\n"
    "Only if the result contains data, choose the narrowest reader set present in it. "
    "The question is who may read the bytes the call returns to the agent, never "
    "where the call sends data."
)

DELTA_TRUST_CRITERIA = {
    "trusted": (
        "The returned content was authored by the operator or produced by "
        "deterministic computation over the operator's own inputs: reading the "
        "project's own source, running its tests, inspecting local git state, or a "
        "first-party service under the operator's control. `trusted` describes "
        "provenance, not factual correctness."
    ),
    "suspicious": (
        "The returned content could have been authored or influenced by a party "
        "outside the operator's control, so it could carry a prompt injection: a "
        "fetched web page or search result, third-party package metadata or a "
        "downloaded artifact, an inbound email, chat message or support ticket "
        "written by someone else, or the free-form output of another model or agent. "
        "The agent's own harness surfaces — loading a tool schema, invoking a skill, "
        "reading its own scratch files — are not third-party content."
    ),
}

DELTA_TRUST_INSTRUCTIONS = (
    f"{FRAME}\n\n"
    "Decide `delta.trust`: the trust rank of the data the call RETURNS to the agent. "
    "The chain is `suspicious` < `trusted`. Answer `suspicious` when the returned "
    "content could have been authored or influenced by a party outside the operator's "
    "control, because a later tool that requires `trusted` must not run on it. Answer "
    "about the provenance of what comes back, not about how dangerous the call is to "
    "run.\n\n"
    "Judge by who wrote the returned text, not by whether the call used the network. "
    "What a command or its target service reports about the call itself — a push's "
    "progress lines, the URL or id of what it created, a list of releases or check "
    "states, an exit status — is `trusted`. Text a third party wrote — a fetched page, "
    "a downloaded file, an issue or message body, a package manager's install log — is "
    "`suspicious`."
)

REQUIRES_AUDIENCE_CRITERIA = {
    "none": (
        "No bytes leave this machine and no other reader receives anything: the call "
        "reads, searches or computes locally, or writes to a local path — scratch, "
        "temporary, or inside the project's working tree. A working-tree edit is "
        "`none` because the bytes reach a reader only when a later call pushes or "
        "publishes them."
    ),
    "internal": (
        "The receiving party is a person or service inside the operator's "
        "organization: posting to an internal chat channel or sending a direct "
        "message, pushing to a private repository, emailing a colleague, writing to "
        "an internal service, database or ticket — and also a read-only query against "
        "an internal service, because the query itself reaches it."
    ),
    "public": (
        "The receiving party is outside the operator's control: a public repository, "
        "issue tracker or website, a package registry, an external email recipient, "
        "or any third-party service — including a read-only one, because a search "
        "query or an HTTP request reaches that destination."
    ),
}

REQUIRES_AUDIENCE_INSTRUCTIONS = (
    f"{FRAME}\n\n"
    "Decide `requires.audience.contains`: which readers must already be allowed to "
    "read the trajectory's data before this call may run, because the call delivers "
    "data to them.\n\n"
    "Any argument value that leaves this machine is data delivered to whoever "
    "receives it — a query string, a message body, a file's contents, a path in a "
    "URL. Classify by who receives those bytes. A call being a read does not by "
    "itself make it `none`: a search against a remote service delivers the query to "
    "that service.\n\n"
    "Judge the destination the arguments actually name. If the call reaches several, "
    "answer with the widest one."
)

REQUIRES_TRUST_INSTRUCTIONS = (
    f"{FRAME}\n\n"
    "Decide whether this call needs `requires.trust = \"trusted\"`: whether OpenAPPA "
    "must refuse it once the trajectory has read attacker-influenceable data.\n\n"
    "This is an information-flow question, not a damage question. Do not answer yes "
    "merely because a call is destructive or hard to undo — that is an "
    "attention/review requirement, which is a different field."
)

REQUIRES_TRUST_CRITERIA = {
    "true": (
        "The call turns data the trajectory holds into an effect outside the "
        "trajectory: it sends, publishes or delivers bytes off this machine, or it "
        "executes or installs content that the trajectory supplied. Publishing an "
        "artifact, filing a report, installing a package and running a downloaded "
        "script all count. An injected instruction that fires this call moves data or "
        "code outward."
    ),
    "false": (
        "The call's effect stays inside the operator's machine and the trajectory "
        "itself — even if it overwrites or deletes local files. Re-running it after "
        "an injection produces nothing but wasted work."
    ),
}

# Written from the contract examples in the docs, not from observed calls.
EXAMPLES = [
    {
        "tool": "Bash",
        "arguments": {"command": "grep -rn 'fn resolve' src/ | head -20", "description": "Find the resolver"},
        "labels": {"delta_audience": "internal", "delta_trust": "trusted",
                   "requires_audience": "none", "requires_trusted": "false"},
        "why": "reads the working tree; nothing leaves the machine",
    },
    {
        "tool": "WebFetch",
        "arguments": {"url": "https://docs.example.org/guide", "prompt": "summarise the retry policy"},
        "labels": {"delta_audience": "public", "delta_trust": "suspicious",
                   "requires_audience": "public", "requires_trusted": "true"},
        "why": "a public page comes back, and the URL reaches a third party",
    },
    {
        "tool": "Bash",
        "arguments": {"command": "cat ~/.aws/credentials", "description": "Show the profile"},
        "labels": {"delta_audience": "self", "delta_trust": "trusted",
                   "requires_audience": "none", "requires_trusted": "false"},
        "why": "returns secrets, but reads them locally",
    },
    {
        "tool": "send_email",
        "arguments": {"recipient": "colleague@corp.example", "body": "The migration finished."},
        "labels": {"delta_audience": "public", "delta_trust": "trusted",
                   "requires_audience": "internal", "requires_trusted": "true"},
        "why": "only an acknowledgement comes back; the body is delivered to a colleague",
    },
    {
        "tool": "Write",
        "arguments": {"file_path": "/tmp/report.md", "content": "# Findings\n\nThe cache is cold on boot."},
        "labels": {"delta_audience": "public", "delta_trust": "trusted",
                   "requires_audience": "none", "requires_trusted": "false"},
        "why": "an acknowledgement comes back, and the write stays local",
    },
    {
        "tool": "Bash",
        "arguments": {"command": "git push origin HEAD:main", "description": "Publish the branch"},
        "labels": {"delta_audience": "public", "delta_trust": "trusted",
                   "requires_audience": "public", "requires_trusted": "true"},
        "why": "the commits reach a public remote",
    },
    {
        "tool": "get_ticket_from_crm",
        "arguments": {"ticket_id": "T-4471"},
        "labels": {"delta_audience": "internal", "delta_trust": "suspicious",
                   "requires_audience": "internal", "requires_trusted": "false"},
        "why": "a customer record comes back, written by someone outside the operator's control; the ticket id reaches an internal service",
    },
    {
        "tool": "slack_read_channel",
        "arguments": {"channel_id": "C0123", "limit": 50},
        "labels": {"delta_audience": "internal", "delta_trust": "suspicious",
                   "requires_audience": "internal", "requires_trusted": "true"},
        "why": "an internal chat log comes back, and the read itself reaches the workspace",
    },
    {
        "tool": "Bash",
        "arguments": {"command": "gh pr create --title 'Fix the retry loop' --body-file notes.md",
                      "description": "Open the pull request"},
        "labels": {"delta_audience": "public", "delta_trust": "trusted",
                   "requires_audience": "public", "requires_trusted": "true"},
        "why": "the service answers with the new pull request's URL; the title and body reach a hosted repository",
    },
    {
        "tool": "Bash",
        "arguments": {"command": "npm install left-pad 2>&1 | tail -3", "description": "Add the dependency"},
        "labels": {"delta_audience": "public", "delta_trust": "suspicious",
                   "requires_audience": "public", "requires_trusted": "true"},
        "why": "a registry receives the request, and the package it serves is installed and its log comes back",
    },
    {
        "tool": "ToolSearch",
        "arguments": {"query": "select:Read,Edit", "max_results": 5},
        "labels": {"delta_audience": "public", "delta_trust": "trusted",
                   "requires_audience": "none", "requires_trusted": "false"},
        "why": "the harness returns its own tool schemas; nothing leaves the machine",
    },
    {
        "tool": "execute_remedy_plan",
        "arguments": {"offer_id": "5f1c02a9d3e84b77"},
        "labels": {"delta_audience": "public", "delta_trust": "trusted",
                   "requires_audience": "none", "requires_trusted": "false"},
        "why": "the harness answers with a status; the offer id reaches no outside reader",
    },
    {
        "tool": "Bash",
        "arguments": {"command": "cargo test -p engine 2>&1 | tail -20", "description": "Run the suite"},
        "labels": {"delta_audience": "internal", "delta_trust": "trusted",
                   "requires_audience": "none", "requires_trusted": "false"},
        "why": "a test run over the operator's own source",
    },
]

QUESTIONS = {
    "delta_audience": ("choice", DELTA_AUDIENCE_INSTRUCTIONS, DELTA_AUDIENCE_CRITERIA),
    "delta_trust": ("choice", DELTA_TRUST_INSTRUCTIONS, DELTA_TRUST_CRITERIA),
    "requires_audience": ("choice", REQUIRES_AUDIENCE_INSTRUCTIONS, REQUIRES_AUDIENCE_CRITERIA),
    "requires_trusted": ("noul", REQUIRES_TRUST_INSTRUCTIONS, REQUIRES_TRUST_CRITERIA),
}


def worked_examples(label: str) -> str:
    lines = ["Worked examples:"]
    for example in EXAMPLES:
        call = json.dumps({"tool": example["tool"], "arguments": example["arguments"]}, ensure_ascii=False)
        lines.append(f"- {call}\n  -> {example['labels'][label]}  ({example['why']})")
    return "\n".join(lines)


def questions(hint: str | None) -> dict[str, dict]:
    """The request's `questions`: each instruction, the deployer's hint when
    the policy declares one, then the worked examples for that label."""
    rendered = {}
    for label, (kind, instructions, criteria) in QUESTIONS.items():
        parts = [instructions]
        if hint:
            parts.append(f"The deployer's instruction for this tool: {hint}")
        parts.append(worked_examples(label))
        rendered[label] = {"type": kind, "instructions": "\n\n".join(parts), "criteria": criteria}
    return rendered
