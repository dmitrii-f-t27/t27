"""tri queue ranks open issues only as specs/queen/priority.t27 decides (#6366).

Three halves:
  1. the spec's own tests, built by the system cc from gen/c/queen/priority.c;
  2. queue.rank on fixtures for every rule: no labels keeps the listing order,
     an unlabelled issue never beats an urgent one (stokowski sorted Linear's
     "no priority" 0 ahead of "urgent" 1), the fourth critical runs as high, a
     blocked issue is skipped, aged low work loses the tie to real high work,
     and conflicting labels take the more urgent level;
  3. mutation control: a copy of the C whose outranks compares the wrong way
     must fail half 2, or half 2 proves nothing.
No network, no t27c.
"""

import importlib.util
import os
import subprocess
import sys
import tempfile
from datetime import datetime, timedelta, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
# Loaded by path under its own name: `queue` is also a stdlib module.
_spec = importlib.util.spec_from_file_location("tri_queue", ROOT / "scripts" / "tri_loop" / "queue.py")
q = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(q)

NOW = datetime(2026, 10, 5, tzinfo=timezone.utc)


def issue(n, labels=(), age=0, blocked=0):
    created = (NOW - timedelta(days=age)).isoformat().replace("+00:00", "Z")
    return {"number": n, "title": f"issue {n}", "labels": [{"name": l} for l in labels],
            "created_at": created, "issue_dependencies_summary": {"blocked_by": blocked}}


def order(issues):
    return [r["number"] for r in q.rank(issues, NOW) if r["eligible"]]


def fixtures():
    """Returns the failed rule names; empty means every rule held."""
    failed = []

    def check(name, cond):
        if not cond:
            failed.append(name)

    check("no labels keep the listing order", order([issue(3), issue(2), issue(1)]) == [3, 2, 1])
    check("unlabelled never beats urgent",
          order([issue(9), issue(8), issue(7, ["priority/critical"])])[0] == 7)
    check("P1 runs before unlabelled, after critical",
          order([issue(1), issue(2, ["high-priority"]), issue(3, ["P0"])]) == [3, 2, 1])
    rows = q.rank([issue(n, ["P0"]) for n in (1, 2, 3, 4)], NOW)
    fourth = [r for r in rows if r["number"] == 4][0]
    check("the fourth critical runs as high", q.LEVELS[fourth["level"]] == "HIGH" and fourth["why"] == "capped")
    rows = q.rank([issue(1), issue(2, ["P0"], blocked=1)], NOW)
    check("a blocked issue is skipped", order([issue(1), issue(2, ["P0"], blocked=1)]) == [1]
          and [r for r in rows if r["number"] == 2][0]["why"] == "blocked")
    check("aged medium work loses the tie to real high work",
          order([issue(1, ["priority/medium"], age=60), issue(2, ["priority/high"])]) == [2, 1])
    check("aged medium work passes unlabelled work",
          order([issue(1), issue(2, ["priority/medium"], age=60)]) == [2, 1])
    check("aged low work never passes unlabelled work",
          order([issue(1), issue(2, ["priority/low"], age=600)]) == [1, 2])
    check("aged low work passes fresh low work",
          order([issue(1, ["priority/low"]), issue(2, ["priority/low"], age=60)]) == [2, 1])
    check("fresh low work runs after unlabelled work",
          order([issue(1, ["priority/low"]), issue(2)]) == [2, 1])
    check("conflicting labels take the more urgent level",
          q.LEVELS[q.rank([issue(1, ["P3", "priority/high"])], NOW)[0]["base"]] == "HIGH")
    return failed


def main():
    gen = q.GEN
    with tempfile.TemporaryDirectory() as tmp:
        exe = Path(tmp) / "priority_tests"
        p = subprocess.run([os.environ.get("CC", "cc"), "-DT27_TEST_MAIN", "-O2", "-w", "-o", str(exe), str(gen)],
                           capture_output=True, text=True)
        if p.returncode != 0 or subprocess.run([str(exe)], capture_output=True).returncode != 0:
            print(f"FAIL  the spec's own tests did not pass from {gen.relative_to(ROOT)}: {p.stderr[:300]}")
            return 1
        print("ok    the spec's own tests pass from the generated C")

        failed = fixtures()
        for name in failed:
            print(f"FAIL  {name}")
        if failed:
            return 1
        print("ok    every ranking rule holds through tri queue")

        src = gen.read_text()
        needle = "if ((eff_a < eff_b)) {"
        if needle not in src:
            print("FAIL  mutation control cannot find outranks' first comparison; update this test")
            return 1
        mutant = Path(tmp) / "priority.c"
        mutant.write_text(src.replace(needle, "if ((eff_a > eff_b)) {", 1))
        q.GEN = mutant
        q.rules.cache_clear()
        os.environ["XDG_CACHE_HOME"] = tmp
        if not fixtures():
            print("FAIL  mutation control: a reversed outranks passed the fixtures, so they prove nothing")
            return 1
        print("ok    mutation control: a reversed outranks is caught")
    return 0


if __name__ == "__main__":
    sys.exit(main())
