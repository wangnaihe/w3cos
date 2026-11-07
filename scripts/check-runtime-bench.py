#!/usr/bin/env python3
"""Validate runtime-bench evidence without imposing absolute timing budgets."""

import argparse
import json
import math
from pathlib import Path
import sys


class InvalidReport(ValueError):
    """The harness report is incomplete or fails a same-run sanity check."""


def require(condition, message):
    if not condition:
        raise InvalidReport(message)


def load_report(text):
    """Reject duplicate keys and nonstandard JSON constants before validation."""
    def unique_object(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, f"duplicate JSON key: {key}")
            result[key] = value
        return result

    def reject_constant(value):
        raise InvalidReport(f"nonfinite JSON constant: {value}")

    return json.loads(text, object_pairs_hook=unique_object, parse_constant=reject_constant)


def number(value, label, minimum=0, integer=False):
    valid_type = type(value) is int if integer else type(value) in (int, float)
    require(valid_type, f"{label} must be a number, not a bool/string")
    # Integers are always finite; math.isfinite can overflow on large JSON ints.
    require(type(value) is int or math.isfinite(value), f"{label} must be finite")
    require(value >= minimum, f"{label} must be >= {minimum}")
    return value


def validate_report(report):
    """Apply the CI gate with explicit types and finite, nonnegative evidence."""
    require(isinstance(report, dict), "report must be an object")

    def section(name):
        value = report.get(name)
        require(isinstance(value, dict), f"{name} must be an object")
        return value

    def field(data, name, label, minimum=0, integer=False):
        return number(data.get(name), f"{label}.{name}", minimum, integer)

    def positive(data, name, label):
        value = field(data, name, label)
        require(value > 0, f"{label}.{name} must be positive")
        return value

    style = section("style_layout")
    nodes = field(style, "nodes", "style_layout", 5000, integer=True)
    full = positive(style, "median_us", "style_layout")
    for name in ("width", "background", "text", "class", "insert", "remove"):
        label = f"incremental_{name}"
        edit = section(label)
        elapsed = positive(edit, "median_us", label)
        require(elapsed < full * 4, f"{label} is not faster than four full passes")
        if name != "background":
            require(edit.get("kind") in ("style-subtree", "splice"), f"{label} did not use subtree layout")
            solved = field(edit, "solved_nodes", label, integer=True)
            require(solved * 4 < nodes, f"{label} solved too much of the tree")
        if name in ("width", "class", "text"):
            dimension = "height" if name == "text" else "width"
            before = field(edit, f"before_{dimension}", label)
            after = field(edit, f"after_{dimension}", label)
            require(before != after, f"{label} did not change {dimension}")
        elif name in ("insert", "remove"):
            before = field(edit, "children_before", label, integer=True)
            after = field(edit, "children_after", label, integer=True)
            delta = 1 if name == "insert" else -1
            require(after == before + delta, f"{label} has incorrect child counts")
        else:
            require(edit.get("sibling_moved") is False, "background edit moved a sibling or lacks boolean evidence")

    paint = section("paint_scroll")
    field(paint, "rows", "paint_scroll", 200, integer=True)
    positive(paint, "first_us", "paint_scroll")
    field(paint, "checksum", "paint_scroll", 1, integer=True)
    positive(section("host_calls"), "median_ns_per_call", "host_calls")
    positive(section("startup"), "wall_us", "startup")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("report", type=Path)
    args = parser.parse_args(argv)
    try:
        validate_report(load_report(args.report.read_text()))
    except (OSError, ValueError) as error:
        print(f"runtime bench sanity failed: {error}", file=sys.stderr)
        return 1
    print("runtime bench sanity ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
