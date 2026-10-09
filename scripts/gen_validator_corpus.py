#!/usr/bin/env python3
"""Record pydantic's verdict on a fixed set of parameter probes.

The Rust validator is checked against this file, so a change to a model that
changes what pydantic accepts or how it phrases a refusal fails
``--check`` rather than drifting unnoticed. Run it from the repository root:

    uv run python scripts/gen_validator_corpus.py
    uv run python scripts/gen_validator_corpus.py --check

The probes are derived from each operation's parameter schema: one valid call
with only the required fields, then one probe per way a field can be wrong.
What is recorded is pydantic's own answer, never a hand-written expectation.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
CORPUS = REPO_ROOT / "tests" / "parity" / "validator_corpus.json"

# pydantic appends this to every complaint. It names a version and a URL that
# move when pydantic does, so the comparison is of the message, not of it.
TAIL = re.compile(r" \[type=.*?\]")
DOC_LINK = re.compile(r"\n {4}For further information visit.*")


def base_value(prop: dict, defs: dict):
    if "$ref" in prop:
        return base_obj(defs.get(prop["$ref"].split("/")[-1], {}), defs)
    if "anyOf" in prop:
        for branch in prop["anyOf"]:
            if branch.get("type") != "null":
                return base_value(branch, defs)
        return None
    if "enum" in prop:
        return prop["enum"][0]
    if "const" in prop:
        return prop["const"]
    kind = prop.get("type")
    if kind == "string":
        return "x" * max(prop.get("minLength", 1), 1)
    if kind == "integer":
        if "minimum" in prop or "exclusiveMinimum" in prop:
            return max(prop.get("minimum", 1), prop.get("exclusiveMinimum", 0) + 1)
        return 1
    if kind == "number":
        return prop.get("minimum", 1.0)
    if kind == "boolean":
        return True
    if kind == "array":
        item = prop.get("items", {})
        return [base_value(item, defs) for _ in range(prop.get("minItems", 0))]
    if kind == "object":
        return base_obj(prop, defs) if "properties" in prop else {}
    return "x"


def base_obj(schema: dict, defs: dict) -> dict:
    required = schema.get("required", [])
    return {
        name: base_value(field, defs)
        for name, field in schema.get("properties", {}).items()
        if name in required
    }


def scalar(prop: dict, defs: dict) -> dict:
    """The branch of a field that describes the value, past a null union or a ref."""
    if "$ref" in prop:
        return defs.get(prop["$ref"].split("/")[-1], {})
    if "anyOf" in prop:
        for branch in prop["anyOf"]:
            if branch.get("type") != "null":
                return scalar(branch, defs)
    return prop


def probes_for(schema: dict) -> list[tuple[str, dict]]:
    defs = schema.get("$defs", {})
    props = schema.get("properties", {})
    required = schema.get("required", [])
    good = base_obj(schema, defs)
    probes = [("valid-minimal", good), ("empty", {}), ("extra-key", {**good, "zz_extra": 1})]
    for name in required:
        probes.append((f"missing:{name}", {key: value for key, value in good.items() if key != name}))
    for name, prop in props.items():
        field = scalar(prop, defs)
        kind = field.get("type")

        def add(tag: str, value: object, name: str = name) -> None:
            probes.append((f"{name}:{tag}", {**good, name: value}))

        # Every field, nullable or not. A nullable one must accept null; one that
        # is not must refuse it. Skipping the nullable ones is how a null on a
        # nullable enum went untested.
        add("null", None)
        add("valid", base_value(prop, defs))
        if "enum" in field:
            # A member that is not allowed, and the right member in the wrong
            # case. Both must be refused, whether or not the field is nullable.
            add("enum-bad", "zzz-bad")
            add("enum-case", str(field["enum"][0]).swapcase())
        if kind == "integer":
            for tag, value in [("str5", "5"), ("float5.0", 5.0), ("float5.5", 5.5),
                               ("bool", True), ("strjunk", "abc"), ("str-space", " 5 "),
                               ("huge", 10**30)]:
                add(tag, value)
            if "minimum" in field:
                add("min-1", field["minimum"] - 1)
                add("min", field["minimum"])
            if "maximum" in field:
                add("max+1", field["maximum"] + 1)
                add("max", field["maximum"])
            if "exclusiveMinimum" in field:
                add("exmin", field["exclusiveMinimum"])
        elif kind == "number":
            for tag, value in [("str", "1.5"), ("int", 2), ("bool", True)]:
                add(tag, value)
            if "minimum" in field:
                add("min-1", field["minimum"] - 1)
            if "maximum" in field:
                add("max+1", field["maximum"] + 1)
        elif kind == "boolean":
            for tag, value in [("int1", 1), ("int0", 0), ("int2", 2), ("strtrue", "true"),
                               ("stryes", "yes"), ("strjunk", "maybe"), ("float1", 1.0)]:
                add(tag, value)
        elif kind == "string" or "enum" in field:
            add("int", 5)
            add("bool", True)
            if "minLength" in field:
                add("minlen-1", "é" * (field["minLength"] - 1))
                add("ws", " " * max(field["minLength"], 1))
            if "maxLength" in field:
                add("maxlen+1", "é" * (field["maxLength"] + 1))
                add("maxlen", "é" * field["maxLength"])
            if "pattern" in field:
                add("pattern-bad", "!!bad pattern!!")
            add("unicode", "é😀")
        elif kind == "array":
            add("notlist", "a,b")
            add("dict", {"a": 1})
            if "minItems" in field:
                add("minitems-1", [])
            item = field.get("items", {})
            add("item-int", [5])
            if "$ref" in item or item.get("type") == "object":
                add("item-bad-obj", [{"zz": 1}])
        if field.get("properties") or kind == "object":
            add("obj-str", "x")
            add("obj-extra", {"zz_extra": 1})
            add("obj-empty", {})
    if len(required) >= 2:
        dropped = {key: value for key, value in good.items() if key not in required[:2]}
        probes.append(("two-missing", dropped))
    return probes


def verdict(operation, raw: dict) -> dict:
    import pydantic

    try:
        model = operation.params_model.model_validate(raw)
    except pydantic.ValidationError as exc:
        return {
            "ok": False,
            "text": f"invalid arguments for {operation.name}:\n{exc}".rstrip(),
            # The parameters as sent, so a number past 2^63 keeps its digits.
            # JSON parsing rounds it, and the comparison puts the digits back.
            "sent": json.dumps(raw, ensure_ascii=False),
            # loc mixes strings and ints, and type is the error code. Both are
            # what FastAPI's 422 detail repeats, so the comparison can check
            # them instead of parsing them back out of the text.
            "errors": [
                {"loc": list(err["loc"]), "type": err["type"]} for err in exc.errors()
            ],
        }
    return {"ok": True, "dump": model.model_dump(mode="json")}


def build() -> list[dict]:
    sys.path.insert(0, str(REPO_ROOT / "src"))
    from vogt.registry.operations import build_operations

    corpus = []
    for operation in build_operations():
        schema = operation.params_model.model_json_schema()
        for tag, raw in probes_for(schema):
            corpus.append({"op": operation.name, "tag": tag, "params": raw, "py": verdict(operation, raw)})
    return corpus


def main() -> int:
    check = "--check" in sys.argv[1:]
    corpus = build()
    rendered = json.dumps(corpus, indent=1, ensure_ascii=False) + "\n"
    if check:
        current = CORPUS.read_text(encoding="utf-8") if CORPUS.exists() else ""
        if current != rendered:
            print(f"{CORPUS.relative_to(REPO_ROOT)} is stale; re-run scripts/gen_validator_corpus.py", file=sys.stderr)
            return 1
        print(f"validator corpus matches pydantic ({len(corpus)} probes)")
        return 0
    CORPUS.write_text(rendered, encoding="utf-8")
    print(f"wrote {len(corpus)} probes to {CORPUS.relative_to(REPO_ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
