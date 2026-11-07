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


# Legacy engine (`W3COS_LAYOUT_NG=0`) checksums recorded on the full-gate
# box. Do not replace these with NG-on numbers when the default engine
# changes. Order is large-layout rects, startup layout rects, startup
# paint pixels, scroll paint pixels.
# The scroll value is the first list frame plus exactly three 24px scrolls.
# It is not the sum of every `--samples` timing frame.
LEGACY_LAYOUT_NG_OFF_CHECKSUMS = (
    (("style_layout", "layout_checksum"), 18294915160932),
    (("startup", "child", "layout_checksum"), 31627714668),
    (("startup", "child", "checksum"), 150112521),
    (("paint_scroll", "checksum"), 1786374846),
)


def nested(report, path):
    value = report
    label = []
    for key in path:
        label.append(key)
        require(isinstance(value, dict), f"{'.'.join(label)} is missing")
        value = value.get(key)
    return value, ".".join(label)


def expect_legacy_checksums(report):
    """The NG-off lane must still produce the historical checksum tuple."""
    require(report.get("layout_ng") == "off", "legacy checksum lane requires layout_ng=off")
    frames, frames_label = nested(report, ("paint_scroll", "checksum_frames"))
    require(frames == 3, f"{frames_label} must be the historical 3-scroll protocol")
    for path, expected in LEGACY_LAYOUT_NG_OFF_CHECKSUMS:
        value, label = nested(report, path)
        require(type(value) is int, f"{label} must be an integer")
        require(value == expected, f"{label} is {value}, legacy constant is {expected}")


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
    # Every mode, including LayoutNG on, edits a column flex through the
    # subtree. A whole-tree rebuild is not that edit. `retained` is a
    # cached full compute and is not one either.
    layout_ng = report.get("layout_ng", "off")
    require(layout_ng in ("off", "on", "verify"), f"layout_ng {layout_ng!r} is unknown")
    for name in ("width", "background", "text", "class", "insert", "remove"):
        label = f"incremental_{name}"
        edit = section(label)
        elapsed = positive(edit, "median_us", label)
        # A cap against a runaway sample, not a claim that the modes match.
        require(elapsed < full * 4, f"{label} is not faster than four full passes")
        if name == "background":
            kind = edit.get("kind")
            require(kind == "paint", f"{label} kind {kind!r} is not paint")
        else:
            solved = field(edit, "solved_nodes", label, integer=True)
            kind = edit.get("kind")
            require(
                kind in ("style-subtree", "splice"),
                f"{label} did not use subtree layout",
            )
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
    # Three scroll frames, on every lane. The pixel tuple checked by
    # --expect-legacy-checksums is the box font environment; this runner
    # check is the protocol, and it does not replace those constants.
    frames = field(paint, "checksum_frames", "paint_scroll", 1, integer=True)
    require(frames == 3, f"paint_scroll.checksum_frames is {frames}, want 3")
    positive(section("host_calls"), "median_ns_per_call", "host_calls")
    positive(section("startup"), "wall_us", "startup")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("report", type=Path)
    parser.add_argument(
        "--expect-legacy-checksums",
        action="store_true",
        help="require the historical W3COS_LAYOUT_NG=0 checksum tuple",
    )
    args = parser.parse_args(argv)
    try:
        report = load_report(args.report.read_text())
        validate_report(report)
        if args.expect_legacy_checksums:
            expect_legacy_checksums(report)
    except (OSError, ValueError) as error:
        print(f"runtime bench sanity failed: {error}", file=sys.stderr)
        return 1
    print("runtime bench sanity ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
