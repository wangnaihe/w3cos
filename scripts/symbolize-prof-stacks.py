#!/usr/bin/env python3
"""Symbolize sampled stacks for `split_workspace --hot-from-stacks`.

Input (stdin or FILE): one sample per line, frames separated by `;`,
innermost first, each frame `module+hexoffset` (e.g. `libw3cos_seg_6.so+1a2b3c`).
Frames that are not in that form are passed through. Leading frames
(`--skip N`) can be dropped, e.g. a sampler's own base frames.

Output: the same stacks with demangled symbols, one sample per line:
    w3cos_seg_6::m189::m189_Foo__ctor;libc.so.6+3fd6f

usage: symbolize-prof-stacks.py [--skip N] LIB_DIR [LIB_DIR...] < samples > stacks
"""
import os
import re
import subprocess
import sys
from collections import defaultdict


def main(argv):
    skip = 0
    if argv and argv[0] == "--skip":
        skip = int(argv[1])
        argv = argv[2:]
    dirs = argv
    if not dirs:
        sys.exit(__doc__)
    samples = [
        line.strip().strip(";").split(";")[skip:] for line in sys.stdin if line.strip()
    ]
    frame = re.compile(r"^([^+;]+)\+([0-9a-fA-F]+)$")
    need = defaultdict(set)
    for stack in samples:
        for f in stack:
            m = frame.match(f)
            if m:
                need[m.group(1)].add(m.group(2))
    symbols = {}
    for module, offsets in need.items():
        path = next(
            (os.path.join(d, module) for d in dirs if os.path.exists(os.path.join(d, module))),
            None,
        )
        if path is None:
            continue
        offsets = sorted(offsets)
        out = subprocess.run(
            ["addr2line", "-f", "-C", "-e", path] + ["0x" + o for o in offsets],
            capture_output=True,
            text=True,
        ).stdout.split("\n")
        for i, offset in enumerate(offsets):
            name = out[2 * i] if 2 * i < len(out) else "??"
            if name and name != "??":
                symbols[(module, offset)] = re.sub(r"::h[0-9a-f]{16}$", "", name).replace(";", ",")
    for stack in samples:
        names = []
        for f in stack:
            m = frame.match(f)
            names.append(symbols.get((m.group(1), m.group(2)), f) if m else f)
        print(";".join(names))


if __name__ == "__main__":
    main(sys.argv[1:])
