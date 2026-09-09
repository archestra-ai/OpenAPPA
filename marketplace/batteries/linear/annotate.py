"""Deterministic Linear contracts; deployment-owned JSON hint supplies resource ACLs.

This helper never contacts Linear, interprets prose, or trusts MCP annotations.
Every supplied scope argument must be fixed by the matching operator rule.
"""

import json
from pathlib import Path
import sys
from linear_schema import check_schema, valid

ROOT = Path(__file__).resolve().parent
OPERATIONS = json.loads((ROOT / "operations.json").read_text())["operations"]
SCHEMAS = {t["name"]: t["inputSchema"] for t in json.loads((ROOT / "schemas.json").read_text())["surfaces"]["read-write"]["tools"]}
for _schema in SCHEMAS.values():
    check_schema(_schema)
PROFILES = {"read-only", "team-use", "approved-writes", "production-lockdown"}
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
            if (not isinstance(rule, dict) or set(rule) - {"match", "audience", "production"}
                    or not {"match", "audience"}.issubset(rule)
                    or not isinstance(rule["match"], dict)
                    or set(rule["match"]) - SCHEMAS[tool].get("properties", {}).keys()
                    or type(rule.get("production", False)) is not bool):
                raise Refusal("invalid resource rule")
            audience = rule["audience"]
            if audience != "public" and (not isinstance(audience, list) or not audience
                    or not all(isinstance(a, str) and a and a != "public" for a in audience)):
                raise Refusal("invalid audience")
    return config


def annotate(request):
    if (not isinstance(request, dict) or type(request.get("version")) is not int
            or request["version"] != 1 or request.get("kind") != "annotation"):
        raise Refusal("unsupported annotation envelope")
    name = request.get("name", "")
    if name not in {"linear." + profile for profile in PROFILES}:
        raise Refusal("unknown Linear annotator")
    profile = name.removeprefix("linear.")
    declaration = request.get("declaration")
    if not isinstance(declaration, dict):
        raise Refusal("missing declaration")
    config = config_from(declaration)
    artifact = request.get("artifact", {})
    call = artifact.get("args") if isinstance(artifact, dict) else None
    if not isinstance(call, dict) or not isinstance(call.get("arguments"), dict):
        raise Refusal("missing tool call")
    canonical = call.get("name", "")
    parts = canonical.split("/") if isinstance(canonical, str) else []
    if len(parts) != 3 or parts[0] != "mcp" or not parts[1]:
        raise Refusal("call is not a canonical MCP tool")
    # The runtime binds the exact tool to its configured server alias. The
    # consult carries that physical identity, which can be any host namespace.
    tool = parts[2]
    if tool not in OPERATIONS:
        raise Refusal("operation was not reviewed")
    operation = OPERATIONS[tool]
    arguments = call["arguments"]
    schema = SCHEMAS[tool]
    if set(arguments) - schema.get("properties", {}).keys() or not set(schema.get("required", [])).issubset(arguments):
        raise Refusal("unknown or missing arguments")
    if not valid(schema, arguments):
        raise Refusal("arguments do not satisfy the captured schema")
    # APPA's parameter schema is intentionally narrower than JSON Schema. The
    # complete pinned schema is checked here before any policy decision.
    candidates = []
    supplied_scope = set(operation["scope_arguments"]) & arguments.keys()
    for rule in config["rules"].get(tool, []):
        match = rule["match"]
        if (supplied_scope.issubset(match) and all(k in arguments and same(arguments[k], v) for k, v in match.items())):
            candidates.append(rule)
    if len(candidates) != 1:
        raise Refusal("resource audience is unresolved or ambiguous")
    rule = candidates[0]
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


def main():
    try:
        raw = sys.stdin.buffer.read(MAX_BYTES + 1)
        if len(raw) > MAX_BYTES:
            raise Refusal("consult exceeds size limit")
        result = annotate(json.loads(raw))
        json.dump(result, sys.stdout)
        sys.stdout.write("\n")
        return 0
    except (ValueError, TypeError, KeyError):
        print("Linear annotation refused: check the captured schema, profile, and resource audience rules", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
