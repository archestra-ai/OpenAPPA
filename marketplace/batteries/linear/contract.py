"""Policy argument boundaries, not a copy of Linear's API validator.

Scope values (including nested structures) must match operator rules exactly.
Variable values are scalars, except for explicitly reviewed text patch operations.
Linear remains responsible for API formats, ranges and other request validation.
"""
import math

PROFILES = ("approved-writes", "read-only", "team-use", "production-lockdown")
PATCH_FIELDS = {
    "replace": {"op", "old_string", "new_string", "replace_all"},
    "insert_before": {"op", "anchor", "text"},
    "insert_after": {"op", "anchor", "text"},
    "prepend": {"op", "text"},
    "append": {"op", "text"},
    "replace_range": {"op", "from", "to", "new_string"},
}


def argument_names(operation):
    return set(operation["scope_arguments"]) | set(operation["variable_arguments"])


def text_patch(value):
    if not isinstance(value, list):
        return False
    for edit in value:
        if not isinstance(edit, dict) or not isinstance(edit.get("op"), str):
            return False
        fields = PATCH_FIELDS.get(edit["op"])
        if fields is None or set(edit) - fields:
            return False
        if not all(type(v) is bool if k == "replace_all" else isinstance(v, str)
                   for k, v in edit.items()):
            return False
    return True


def valid_arguments(operation, arguments):
    if set(arguments) - argument_names(operation) or not set(operation["required_arguments"]) <= arguments.keys():
        return False
    for key in set(operation["variable_arguments"]) & arguments.keys():
        value = arguments[key]
        if key == "patch":
            if not text_patch(value):
                return False
        elif not (value is None or type(value) in (str, bool, int)
                  or type(value) is float and math.isfinite(value)):
            return False
    return True
