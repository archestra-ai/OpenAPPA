"""Deterministic Linear contracts; deployment-owned JSON hint supplies resource ACLs.

This helper never contacts Linear, interprets prose, or trusts MCP annotations.
Every supplied scope argument must be fixed by the matching operator rule.
"""

import json
from pathlib import Path
import sys
# Installed generations are immutable: importing support code must not create caches.
sys.dont_write_bytecode = True
from contract import PROFILES, argument_names, valid_arguments

ROOT = Path(__file__).resolve().parent
OPERATIONS = json.loads((ROOT / "operations.json").read_text())["operations"]
MAX_BYTES = 8 * 1024 * 1024
# These edits can expand access, cause secondary actions, or apply inherited content.
SENSITIVE_ARGUMENTS = {"team", "teamId", "addTeams", "setTeams", "removeTeams", "delegate", "template",
                       "links", "parentId", "project", "initiative", "addInitiatives", "setInitiatives"}


class Refusal(ValueError):
    pass


def same(left, right):
    return json.dumps(left, sort_keys=True, separators=(",", ":")) == json.dumps(right, sort_keys=True, separators=(",", ":"))


def config_from(declaration):
    hint = declaration.get("hint", '{"rules":{}}')
    config = json.loads(hint)
    if not isinstance(config, dict) or set(config) != {"rules"} or not isinstance(config["rules"], dict):
        raise Refusal("hint must contain a rules object")
    for tool, rules in config["rules"].items():
        if tool not in OPERATIONS or not isinstance(rules, list):
            raise Refusal("configuration names an unknown tool or invalid rules")
        for rule in rules:
            if not isinstance(rule, dict) or set(rule) - {"match", "audience", "production"}:
                raise Refusal("unknown resource rule fields")
            if not {"match", "audience"}.issubset(rule):
                raise Refusal("resource rule needs match and audience")
            if not isinstance(rule["match"], dict) or set(rule["match"]) - argument_names(OPERATIONS[tool]):
                raise Refusal("resource rule names unknown arguments")
            if type(rule.get("production", False)) is not bool:
                raise Refusal("production must be a boolean")
            audience = rule["audience"]
            if audience != "public" and (not isinstance(audience, list) or not audience
                    or not all(isinstance(a, str) and a and a != "public" for a in audience)):
                raise Refusal("invalid audience")
    return config


def resource_rule(config, tool, arguments):
    """Choose exactly one audience rule, binding every supplied scope argument."""
    operation = OPERATIONS[tool]
    candidates = []
    supplied_scope = set(operation["scope_arguments"]) & arguments.keys()
    for rule in config["rules"].get(tool, []):
        match = rule["match"]
        if (supplied_scope.issubset(match) and all(k in arguments and same(arguments[k], v) for k, v in match.items())):
            candidates.append(rule)
    if len(candidates) != 1:
        raise Refusal("resource audience is unresolved or ambiguous")
    return candidates[0]


def decision(tool, arguments, profile, rule):
    """Linear policy: mapped audience, low-trust results and reviewed mutations."""
    operation = OPERATIONS[tool]
    mutation = operation["kind"] != "read"
    if mutation and profile == "read-only":
        raise Refusal("read-only profile refuses mutations")
    if mutation and profile == "production-lockdown" and not rule.get("production", False):
        raise Refusal("mutation has not been enabled for production")
    sensitive = operation["kind"] == "sensitive" or bool(SENSITIVE_ARGUMENTS & arguments.keys())
    attention = []
    if (mutation and (profile != "team-use" or sensitive)) or operation["external_fetch"]:
        attention.append("linear-review")
    audience = rule["audience"]
    requires = {"history": [], "attention": attention,
                "audience": {"contains": "public" if operation["external_fetch"] else audience}}
    if mutation:
        requires["trust"] = "trusted"
    # Mutations can return existing records, and an upload preparation returns a
    # signed URL: their results also enter the mapped audience at low trust.
    delta = {"trust": "suspicious", "audience": audience}
    if tool == "prepare_attachment_upload":
        delta["audience"] = ["self"]
    return {"version": 1, "answer": {"delta": delta, "requires": requires,
                                     "emits": [operation["effect"]] if mutation else []}}


def annotate(request):
    # The runtime constructs the consult envelope and binds the physical server.
    # Reject incompatible protocol/name values; missing fields fail at the command boundary.
    if request["version"] != 1 or request["kind"] != "annotation":
        raise Refusal("unsupported annotation envelope")
    name = request["name"]
    if name not in {"linear." + profile for profile in PROFILES}:
        raise Refusal("unknown Linear annotator")
    call = request["artifact"]["args"]
    namespace, server, tool = call["name"].split("/")
    if namespace != "mcp" or not server or tool not in OPERATIONS:
        raise Refusal("unknown Linear tool")
    arguments = call["arguments"]
    if not isinstance(arguments, dict) or not valid_arguments(OPERATIONS[tool], arguments):
        raise Refusal("arguments do not satisfy the reviewed policy contract")
    config = config_from(request["declaration"])
    rule = resource_rule(config, tool, arguments)
    return decision(tool, arguments, name.removeprefix("linear."), rule)


def main():
    try:
        raw = sys.stdin.buffer.read(MAX_BYTES + 1)
        if len(raw) > MAX_BYTES:
            raise Refusal("consult exceeds size limit")
        result = annotate(json.loads(raw))
        json.dump(result, sys.stdout)
        sys.stdout.write("\n")
        return 0
    except (ValueError, TypeError, KeyError, AttributeError):
        print("Linear annotation refused: check the policy contract, profile, and resource audience rules", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
