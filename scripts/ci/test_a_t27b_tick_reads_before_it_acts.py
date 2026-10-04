#!/usr/bin/env python3
"""tri t27b doctor names each anomaly the t27b steward hit by hand (#6112).

Every source comes from a fixture directory (no network, no gh, no lsof), but
the worktrees are real git repositories, because "mid-merge" is a fact about a
.git directory and a fixture string could not prove it is read.

  healthy  lab at the master tip, fresh, mismatch 0; one MERGEABLE t27b PR with
           required checks green; a released claim; a clean worktree; railway
           5.x; no local reference run; a recent ledger row; plus an unrelated
           PR whose body only mentions t27b in passing -> 0 anomalies, exit 0.
           This is the negative control for every code below.
  broken   each code once: lab on a branch, behind, stale, mismatch, a
           reference-only pass, a lab_error, a red cargo test; a CONFLICTING
           PR with a red required check on a non-master base; a dead claim; a
           worktree mid-merge with a process inside it (named) and a dirty one
           with none; railway 4.5.4; a local `t27b corpus --reference` run;
           a quiet ledger -> exit 1.
  unread   lab.json absent -> LAB-UNREADABLE, never "no lab anomalies", and
           no LOCAL-REFERENCE (it is only an anomaly while the lab answers).
  usage    an unknown action -> exit 2.
  honest   (#6184) the card quotes in-reference passes / reference passes,
           splits compile-only passes out, and LAB-FRONTEND-DISAGREES names a
           spec t27b's frontend rejects while the reference passes it.
  delta    per-spec transitions between two lab runs: REGRESSED, NEW-MISMATCH
           and CHECK-LOST exit 1; GAINED and REF-MOVED are reported; the
           default --from is chosen by `finished`, not by sha.

And "never write": the worktrees' git state (HEAD, MERGE_HEAD, status) is the
same before and after.
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
TOOL = os.path.join(ROOT, "scripts", "tri_loop", "t27b.py")
MASTER = "a" * 40
fails = []


def check(cond, what):
    print(("ok      " if cond else "FAIL    ") + what)
    if not cond:
        fails.append(what)


def git(cwd, *args):
    return subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@t", *args], cwd=cwd,
                          capture_output=True, text=True).stdout


def make_repo(path):
    os.makedirs(path)
    git(path, "init", "-q", "-b", "main")
    with open(os.path.join(path, "f.txt"), "w") as f:
        f.write("base\n")
    git(path, "add", "f.txt")
    git(path, "commit", "-q", "-m", "base")


def make_midmerge(path):
    make_repo(path)
    git(path, "checkout", "-q", "-b", "side")
    with open(os.path.join(path, "f.txt"), "w") as f:
        f.write("side\n")
    git(path, "commit", "-qam", "side")
    git(path, "checkout", "-q", "main")
    with open(os.path.join(path, "f.txt"), "w") as f:
        f.write("main\n")
    git(path, "commit", "-qam", "main")
    git(path, "merge", "side")  # conflicts: leaves MERGE_HEAD


def lab(**over):
    d = {"ref": "master", "commit": MASTER, "finished": "2026-10-04T15:00:00Z",
         "steps": {"build_t27b": {"ok": True}, "cargo_test_t27b": {"passed": 27, "failed": 0, "ok": True}},
         "summary": {"files": 1190, "reference_pass": 646, "t27b_pass": 47, "mismatch": 0,
                     "t27b_pass_where_reference_does_not": 0, "reference_lab_error": 0,
                     "crash": 0, "timeout": 0, "t27b_fail": 0},
         "top_blockers": [{"construct": "StructDecl", "first": 393, "all": 1569}]}
    for k, v in over.items():
        if k in d["summary"]:
            d["summary"][k] = v
        else:
            d[k] = v
    return d


def pr(n, title, mergeable="MERGEABLE", base="master", red=False, body=""):
    concl = "FAILURE" if red else "SUCCESS"
    return {"number": n, "title": title, "body": body, "headRefName": f"claude/{n}", "baseRefName": base,
            "mergeable": mergeable,
            "statusCheckRollup": [{"name": "validate", "conclusion": "SUCCESS"},
                                  {"name": "check-linked-issue", "conclusion": concl},
                                  {"name": "parse-ratchet", "conclusion": "SUCCESS"}]}


def write_fixture(d, files):
    os.makedirs(d, exist_ok=True)
    for name, content in files.items():
        if content is None:
            continue
        with open(os.path.join(d, name), "w") as f:
            f.write(content if isinstance(content, str) else json.dumps(content))


def doctor(fx, *extra):
    p = subprocess.run([sys.executable, TOOL, "doctor", "--json", "--fixture", fx, *extra],
                       capture_output=True, text=True)
    try:
        codes = [a["code"] for a in json.loads(p.stdout)]
    except ValueError:
        codes = None
    return p.returncode, codes, p.stdout + p.stderr


def snapshot(paths):
    return [(git(p, "rev-parse", "HEAD"), os.path.exists(os.path.join(p, ".git", "MERGE_HEAD")),
             git(p, "status", "--porcelain")) for p in paths]


with tempfile.TemporaryDirectory() as tmp:
    clean, mid, dirty = (os.path.join(tmp, n) for n in ("clean", "mid", "dirty"))
    make_repo(clean)
    make_midmerge(mid)
    make_repo(dirty)
    with open(os.path.join(dirty, "f.txt"), "a") as f:
        f.write("edit\n")
    before = snapshot([clean, mid, dirty])

    common = {"now.txt": "2026-10-04T16:00:00Z", "master.txt": MASTER, "alive_pids.txt": "4242\n"}
    healthy = os.path.join(tmp, "fx-healthy")
    write_fixture(healthy, {**common,
        "lab.json": lab(),
        "prs.json": [pr(6065, "feat(t27b): casts"), pr(6012, "gen-python", mergeable="CONFLICTING",
                                                          body="mentions t27b once")],
        "claim.json": {"since": "2026-10-04T15:27Z", "pid": 9, "released": True},
        "ledger.md": "| UTC | event | links |\n|---|---|---|\n| 2026-10-04T15:31Z | tick | - |\n",
        "worktrees.txt": clean + "\n",
        "cwd.txt": f"p4242\nn{clean}\n",
        "ps.txt": "  4242 01:00 /usr/bin/cargo build -p t27b\n",
        "railway.txt": "railway 5.63.1"})
    code, codes, out = doctor(healthy)
    check(code == 0 and codes == [], f"healthy: exit 0, no anomalies (got {code} {codes})")

    broken = os.path.join(tmp, "fx-broken")
    write_fixture(broken, {**common,
        "lab.json": lab(ref="claude/t27b-railway-lab", commit="b" * 40, finished="2026-10-03T01:00:00Z",
                        mismatch=2, t27b_pass_where_reference_does_not=1, reference_lab_error=3,
                        steps={"cargo_test_t27b": {"passed": 26, "failed": 1, "ok": False}}),
        "prs.json": [pr(6098, "feat(t27b): lab", mergeable="CONFLICTING", base="claude/x", red=True)],
        "claim.json": {"since": "2026-10-04T12:00Z", "pid": 777},
        "ledger.md": "| 2026-10-04T10:00Z | tick | - |\n",
        "worktrees.txt": f"{mid}\n{dirty}\n",
        "cwd.txt": f"p4242\nn{mid}/src\n",
        "ps.txt": "  5150 03:12:26 /tmp/t27b-target-b/release/t27b corpus specs --blockers --reference /tmp/t27c-ref\n",
        "railway.txt": "railway 4.5.4"})
    code, codes, out = doctor(broken)
    want = ["LAB-NOT-MASTER", "LAB-BEHIND", "LAB-STALE", "LAB-MISMATCH", "LAB-OUTSIDE-REF", "LAB-ERROR",
            "LAB-TESTS-RED", "PR-CONFLICTING", "PR-REQUIRED-RED", "PR-BASE-NOT-MASTER", "CLAIM-DEAD",
            "WORKTREE-MIDOP", "WORKTREE-DIRTY", "RAILWAY-OLD-CLI", "LOCAL-REFERENCE", "LEDGER-QUIET"]
    check(code == 1, f"broken: exit 1 (got {code})")
    for w in want:
        check(codes is not None and codes.count(w) == 1, f"broken: {w} exactly once")
    check(codes is not None and sorted(codes) == sorted(want), f"broken: nothing else ({codes})")
    check("processes inside: 4242" in out, "broken: the mid-merge worktree names the process inside it")
    check("no process inside" in out, "broken: the dirty worktree says nobody is inside")

    # #6230: a lab stuck at checkout has no `finished`; it still ages, and says why it measured nothing
    files = {k: open(os.path.join(healthy, k)).read() for k in os.listdir(healthy)}
    stuck_lab = lab(finished=None, updated="2026-10-04T08:00:00Z",
                    steps={"checkout": {"ok": False, "error": "fatal: could not fetch abc from promisor remote"}})
    stuck_lab.pop("summary")
    stuck = os.path.join(tmp, "fx-stuck")
    write_fixture(stuck, {**files, "lab.json": stuck_lab})
    code, codes, out = doctor(stuck)
    check(code == 1 and codes == ["LAB-STALE", "LAB-CHECKOUT"],
          f"stuck: LAB-STALE from `updated` and LAB-CHECKOUT, not a generic LAB-TESTS-RED (got {code} {codes})")
    check("promisor remote" in out and "redeploy" in out, "stuck: the finding quotes the error and names the fix")
    healed = os.path.join(tmp, "fx-healed")
    write_fixture(healed, {**files, "lab.json": lab(steps={"checkout": {"ok": True, "clone": "recloned",
                                                                         "first_error": "promisor remote"}})})
    code, codes, out = doctor(healed)
    check(code == 1 and codes == ["LAB-RECLONED"], f"healed: LAB-RECLONED alone (got {code} {codes})")
    kept = os.path.join(tmp, "fx-kept")
    write_fixture(kept, {**files, "lab.json": lab(steps={"checkout": {"ok": True, "clone": "kept"}})})
    code, codes, out = doctor(kept)
    check(code == 0 and codes == [], f"kept clone: no anomaly (got {code} {codes})")

    # #6237: LAB-ERROR reads the per-spec records; a fail the reference shares (build_verify) is no alarm
    shared = lab(t27b_fail=1, results=[{"file": "specs/fpga/verification/build_verify.t27",
                                        "t27b": "fail", "reference": "fail"}])
    fx = os.path.join(tmp, "fx-shared-fail")
    write_fixture(fx, {**files, "lab.json": shared})
    code, codes, out = doctor(fx)
    check(code == 0 and codes == [], f"shared fail: no LAB-ERROR (got {code} {codes})")
    own = lab(t27b_fail=1, results=[{"file": "specs/a.t27", "t27b": "fail", "reference": "pass"}])
    fx = os.path.join(tmp, "fx-own-fail")
    write_fixture(fx, {**files, "lab.json": own})
    code, codes, out = doctor(fx)
    check(code == 1 and codes == ["LAB-ERROR"] and "t27b_fail 1" in out,
          f"own fail: LAB-ERROR t27b_fail 1 (got {code} {codes})")

    # mutation control: the same tool over a generated C whose checkout rule never fires misses LAB-CHECKOUT,
    # so the finding comes from the spec, not from a branch in t27b.py
    gen_src = open(os.path.join(ROOT, "gen", "c", "tri", "t27b", "steward.c")).read()
    needle = "uint8_t checkout_code(bool ok, bool recloned) {\n    if ((ok == false)) {"
    check(gen_src.count(needle) == 1, "control: the generated checkout rule is where the control expects it")
    tree = os.path.join(tmp, "mut", "scripts", "tri_loop")
    os.makedirs(tree)
    for name in ("t27b.py", "t27b_rules.py"):
        shutil.copy(os.path.join(ROOT, "scripts", "tri_loop", name), tree)
    gen = os.path.join(tmp, "mut", "gen", "c", "tri", "t27b")
    os.makedirs(gen)
    with open(os.path.join(gen, "steward.c"), "w") as f:
        f.write(gen_src.replace(needle, needle.replace("(ok == false)", "(false)")))
    p = subprocess.run([sys.executable, os.path.join(tree, "t27b.py"), "doctor", "--json", "--fixture", stuck],
                       capture_output=True, text=True)
    try:
        mut_codes = [a["code"] for a in json.loads(p.stdout)]
    except ValueError:
        mut_codes = None
    check(mut_codes == ["LAB-STALE"], f"control: a mutated checkout rule loses LAB-CHECKOUT ({mut_codes})")

    unread = os.path.join(tmp, "fx-unread")
    files = {k: open(os.path.join(broken, k)).read() for k in os.listdir(broken)}
    files.pop("lab.json")
    write_fixture(unread, files)
    code, codes, out = doctor(unread)
    check(code == 1 and codes is not None and "LAB-UNREADABLE" in codes, "unread: LAB-UNREADABLE, exit 1")
    check(codes is not None and "LOCAL-REFERENCE" not in codes, "unread: no LOCAL-REFERENCE without a lab")
    check(codes is not None and not any(c.startswith("LAB-") and c != "LAB-UNREADABLE" for c in codes),
          "unread: no lab finding invented from an absent file")

    p = subprocess.run([sys.executable, TOOL, "bogus"], capture_output=True, text=True)
    check(p.returncode == 2, f"usage: unknown action exits 2 (got {p.returncode})")

    p = subprocess.run([sys.executable, TOOL, "status", "--fixture", healthy], capture_output=True, text=True)
    check(p.returncode == 0 and "t27b passes 47 of the 646 specs the reference passes" in p.stdout and "#6012" not in p.stdout,
          "status: prints the lab totals and drops the PR that only mentions t27b")

    # honest (#6184): the card quotes in-reference passes, never t27b_pass / reference_pass
    def spec(f, t, r, tests=0, inv=0):
        return {"file": f, "t27b": t, "reference": r, "tests": tests, "invariants": inv, "detail": f"{t} detail"}
    honest = os.path.join(tmp, "fx-honest")
    hl = lab(t27b_pass=3, t27b_pass_where_reference_passes=2, t27b_pass_where_reference_does_not=1,
             reference_pass=4)
    hl["results"] = [spec("a.t27", "pass", "pass", tests=2), spec("b.t27", "pass", "pass"),
                     spec("c.t27", "pass", "blocked"), spec("d.t27", "frontend", "pass"),
                     spec("e.t27", "frontend", "blocked")]
    write_fixture(honest, {**{k: open(os.path.join(healthy, k)).read() for k in os.listdir(healthy)},
                           "lab.json": hl})
    p = subprocess.run([sys.executable, TOOL, "status", "--fixture", honest], capture_output=True, text=True)
    check("t27b passes 2 of the 4 specs the reference passes (50.0%)" in p.stdout,
          "honest: the card quotes in-reference passes over reference passes")
    check("t27b 3 /" not in p.stdout and "3 of the 4" not in p.stdout,
          "honest: t27b_pass (which includes out-of-reference passes) is never the numerator")
    check("1 ran a test or invariant, 1 compile-only" in p.stdout, "honest: compile-only passes are split out")
    check("1 t27b pass(es) where the reference fails, 1 frontend reject(s)" in p.stdout,
          "honest: out-of-reference passes and frontend rejects are on their own line")
    code, codes, out = doctor(honest)
    check(codes is not None and codes.count("LAB-FRONTEND-DISAGREES") == 1 and "d.t27" in out and "e.t27" not in out,
          f"honest: LAB-FRONTEND-DISAGREES names only the spec the reference passes ({codes})")
    lab_nr = lab(t27b_pass=5, t27b_pass_where_reference_does_not=2)
    lab_nr["summary"].pop("t27b_pass_where_reference_passes", None)
    nr = os.path.join(tmp, "fx-noresults")
    write_fixture(nr, {**{k: open(os.path.join(healthy, k)).read() for k in os.listdir(healthy)}, "lab.json": lab_nr})
    p = subprocess.run([sys.executable, TOOL, "status", "--fixture", nr], capture_output=True, text=True)
    check("t27b passes 3 of the 646" in p.stdout and "ran a test" not in p.stdout,
          "honest: an old lab without the field subtracts the outside passes; no results, no checked line")

    # delta: per-spec ratchet between two lab runs
    dl = os.path.join(tmp, "fx-delta")
    os.makedirs(os.path.join(dl, "runs"))
    A, B = "f" * 40, "1" * 40  # A sorts after B by sha but finished first: order must come from `finished`
    base = [spec("s1", "pass", "pass", tests=1), spec("s2", "blocked", "pass"), spec("s3", "pass", "pass", tests=2),
            spec("s4", "blocked", "pass"), spec("s5", "blocked", "pass"), spec("s6", "pass", "pass", tests=1)]
    ra = lab(commit=A, finished="2026-10-04T10:00:00Z")
    ra["results"] = [dict(x) for x in base[:4]] + [spec("s5", "blocked", "pass"), spec("s6", "blocked", "pass")]
    rb = lab(commit=B, finished="2026-10-04T12:00:00Z")
    rb["results"] = base
    rc = lab(commit=MASTER, finished="2026-10-04T14:00:00Z")
    rc["results"] = [spec("s1", "blocked", "pass"), spec("s2", "fail", "pass"), spec("s3", "pass", "pass"),
                     spec("s4", "pass", "pass"), spec("s5", "blocked", "blocked"), spec("s6", "pass", "pass", tests=1)]
    for sha, r in ((A, ra), (B, rb)):
        with open(os.path.join(dl, "runs", sha + ".json"), "w") as f:
            json.dump(r, f)
    write_fixture(dl, {"lab.json": rc})

    def run_delta(*extra):
        p = subprocess.run([sys.executable, TOOL, "delta", "--json", "--fixture", dl, *extra],
                           capture_output=True, text=True)
        try:
            j = json.loads(p.stdout)
        except ValueError:
            j = None
        return p.returncode, j, p.stdout + p.stderr
    code, j, out = run_delta()
    got = sorted((f["code"], f["file"]) for f in j["findings"]) if j else None
    check(code == 1, f"delta: a regression exits 1 (got {code})")
    check(got == [("CHECK-LOST", "s3"), ("GAINED", "s4"), ("NEW-MISMATCH", "s2"), ("REF-MOVED", "s5"),
                  ("REGRESSED", "s1")], f"delta: each transition once, s6 unchanged is silent ({got})")
    check(j is not None and j["header"].startswith("from 111111111"),
          "delta: the default --from is the run that finished last before --to, not the last sha")
    code, j, out = run_delta("--from", A, "--to", B)
    got = sorted((f["code"], f["file"]) for f in j["findings"]) if j else None
    check(code == 0 and got == [("GAINED", "s6")], f"delta: gains only exit 0 ({code} {got})")
    code, j, out = run_delta("--from", "0" * 40)
    check(code == 2 and "could not read" in out, f"delta: an absent run is 'could not read', exit 2 ({code})")

    check(snapshot([clean, mid, dirty]) == before, "never write: the worktrees' git state is unchanged")
    # negative control for the snapshot: a change must show
    with open(os.path.join(clean, "g.txt"), "w") as f:
        f.write("x\n")
    check(snapshot([clean, mid, dirty]) != before, "control: an added file changes the snapshot")

print(f"\n{'PASS' if not fails else 'FAIL'}: {len(fails)} failure(s)")
sys.exit(1 if fails else 0)
