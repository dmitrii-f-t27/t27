#!/usr/bin/env python3
"""Mutate exactly one line of a file, asserting its current content first."""
import pathlib
import sys

path, lineno, expect, new = sys.argv[1], int(sys.argv[2]), sys.argv[3], sys.argv[4]
p = pathlib.Path(path)
L = p.read_text(encoding="utf-8").splitlines(keepends=True)
cur = L[lineno - 1]
assert cur == expect + "\n", f"line {lineno} is {cur!r}, expected {expect!r}"
L[lineno - 1] = new + "\n"
p.write_text("".join(L), encoding="utf-8")
print(f"MUTATED line {lineno}: {expect!r} -> {new!r}")
