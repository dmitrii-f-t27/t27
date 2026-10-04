#!/usr/bin/env python3
"""tri t27b -- the t27b steward's tick card (status), anomaly scan (doctor) and per-spec ratchet between lab runs (delta).

WHY THIS EXISTS
---------------
The t27b coverage loop (epic #6063: an hourly steward cron plus lane
subagents) reads its state from seven places at the top of every tick. Read by
hand on 2026-10-04, that state produced four slips in one afternoon (#6112):

  * A tick saw /tmp/t27b-unconflict mid-merge and decided the subagent that
    owned it had died. That subagent was alive. A second subagent was sent into
    the same path, and the first one's uncommitted merge was reverted under it.
    A mid-merge worktree is not a dead one; the processes whose cwd is inside
    it say which.
  * `railway whoami` said Unauthorized after the owner had logged in: the
    first `railway` on PATH was 4.5.4, and the login is read only by 5.x.
  * The lab's numbers were quoted as the project's while its `ref` was a
    branch, not master.
  * A local three-hour reference run kept a Mac at load 700 while the lab
    produced the same denominator in 321 s.

And one slip this tool made itself (#6184): `status` printed `t27b 70 /
reference 648` and the steward posted it. 14 of those 70 passes were specs the
reference fails; the honest figure was 56 of 648. Of those 56, 29 ran no test
and no invariant -- they compiled and checked nothing. The card now prints the
in-reference ratio, splits it into checked and compile-only, and keeps the
out-of-reference passes on their own line.

WHAT IS READ (nothing is written, fetched or pushed)
----------------------------------------------------
  lab        GET <lab>/latest.json                       (fixture: lab.json)
  master     git ls-remote origin refs/heads/master      (master.txt)
  prs        gh pr list --search t27b, open, with checks  (prs.json)
  claim      ~/.local/state/t27b-queen/claim.json         (claim.json)
  pids       kill(pid, 0) for the claim's pid             (alive_pids.txt)
  ledger     the last `| 20..` row of ledger.md           (ledger.md)
  worktrees  every /tmp/t27b-* that is a git worktree     (worktrees.txt)
  cwd        lsof -d cwd -Fpn: which process sits where   (cwd.txt)
  ps         ps -axo pid=,etime=,command=                 (ps.txt)
  railway    `railway --version` of the first on PATH     (railway.txt)
  runs       GET <lab>/runs/ and <lab>/runs/<sha>.json    (runs/<sha>.json)
             -- delta only
  now        the clock                                    (now.txt)

--fixture DIR reads every source from DIR instead (worktrees.txt lists real
paths, which are still inspected with git). A missing fixture file is a source
that could not be read, the same as a failed command -- never an empty answer.

ANOMALY CODES (doctor exits 1 when any is printed, 0 when none, 2 on usage)
---------------------------------------------------------------------------
  LAB-UNREADABLE      latest.json could not be read or parsed
  LAB-NOT-MASTER      the lab built a ref other than master
  LAB-BEHIND          the lab's commit is not the master tip
  LAB-STALE           the lab's run finished more than --stale-hours ago
  LAB-MISMATCH        mismatch > 0: a stop and a report, never a skip
  LAB-OUTSIDE-REF     t27b passes where the reference does not: a reference
                      defect or a vacuous pass; either way not in the number
  LAB-ERROR           lab_error, crash, timeout or t27b fail > 0
  LAB-TESTS-RED       a lab step reported ok=false, or cargo test failed
  PR-CONFLICTING      an open t27b PR cannot merge into its base
  PR-REQUIRED-RED     validate / check-linked-issue / parse-ratchet failed
  PR-BASE-NOT-MASTER  stacked PR: retarget after its parent merges
  CLAIM-DEAD          a claim is held by a pid that is not running
  CLAIM-OLD           a claim is held by a live pid past --claim-minutes
  WORKTREE-MIDOP      a /tmp/t27b-* worktree is mid-merge or mid-rebase
  WORKTREE-DIRTY      a /tmp/t27b-* worktree has uncommitted changes
  RAILWAY-OLD-CLI     the first railway on PATH is older than 5.x
  LOCAL-REFERENCE     a local `t27b corpus ... --reference` run is going
                      while the lab answers
  LEDGER-QUIET        the last ledger row is older than --quiet-hours
  LAB-FRONTEND-DISAGREES  t27b's frontend rejects a spec the reference
                      passes: a disagreement `mismatch` does not count

DELTA (tri t27b delta [--from SHA] [--to SHA]; exit 1 on the first three)
---------------------------------------------------------------------------
Per spec, over the specs the reference passes in BOTH runs:
  REGRESSED     t27b passed in --from and does not in --to
  NEW-MISMATCH  t27b fails (or crashes) where it did not, reference passing
  CHECK-LOST    a pass that ran tests/invariants now runs none
  GAINED        t27b passes now and did not before          (reported)
  REF-MOVED     the reference's own verdict changed          (reported: the
                denominator moved, not t27b)
--to defaults to latest.json; --from to the run before it by `finished`.

WHAT THIS DOES NOT ESTABLISH
----------------------------
  * That a worktree with a process inside is healthy. Only that something is
    there; a WORKTREE-MIDOP with owners is someone's work, not debris.
  * That a worktree with no process inside is abandoned. A subagent between
    two commands has no shell open. Read the age before touching it.
  * That the lab's numbers are right. Only that they are master's, fresh, and
    free of the failure classes above.
  * Anything about PRs that do not match the search `t27b`.
  * That a "checked" pass checks much: one test block counts. Compile-only
    means 0 tests and 0 invariants as the lab reports them.

    tri t27b status                # the tick card
    tri t27b doctor                # anomalies, one per line, with the fix
    tri t27b doctor --json         # the same, machine-readable
    tri t27b delta                 # what moved per spec since the previous lab run
"""
import argparse
import datetime as dt
import glob
import json
import os
import re
import subprocess
import sys
import urllib.request

LAB = "https://t27b-lab-production.up.railway.app"
REPO = "gHashTag/t27"
STATE = os.path.expanduser("~/.local/state/t27b-queen")
REQUIRED = ("validate", "check-linked-issue", "parse-ratchet")
ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))


class Unreadable(Exception):
    pass


def run(argv, cwd=None, timeout=60):
    try:
        p = subprocess.run(argv, cwd=cwd, capture_output=True, text=True, timeout=timeout,
                           env=dict(os.environ, GIT_OPTIONAL_LOCKS="0"))
    except (OSError, subprocess.TimeoutExpired) as e:
        raise Unreadable(f"{argv[0]}: {e}")
    if p.returncode != 0:
        raise Unreadable(f"{' '.join(argv[:3])}: exit {p.returncode}: {p.stderr.strip()[:200]}")
    return p.stdout


class Sources:
    """Every external read goes through here, so a fixture can stand in for it."""

    def __init__(self, fixture=None, lab=LAB, state=STATE):
        self.fixture, self.lab, self.state = fixture, lab, state

    def _fx(self, name):
        path = os.path.join(self.fixture, name)
        if not os.path.exists(path):
            raise Unreadable(f"fixture {name} absent")
        with open(path, encoding="utf-8") as f:
            return f.read()

    def now(self):
        if self.fixture:
            return parse_time(self._fx("now.txt").strip())
        return dt.datetime.now(dt.timezone.utc)

    def lab_json(self):
        if self.fixture:
            text = self._fx("lab.json")
        else:
            try:
                with urllib.request.urlopen(self.lab + "/latest.json", timeout=30) as r:
                    text = r.read().decode("utf-8")
            except Exception as e:  # noqa: BLE001 -- any failure is "could not read"
                raise Unreadable(f"{self.lab}/latest.json: {e}")
        try:
            return json.loads(text)
        except ValueError as e:
            raise Unreadable(f"latest.json: {e}")

    def master(self):
        if self.fixture:
            return self._fx("master.txt").strip()
        out = run(["git", "ls-remote", "origin", "refs/heads/master"], cwd=ROOT)
        return out.split()[0] if out.strip() else ""

    def prs(self):
        if self.fixture:
            prs = json.loads(self._fx("prs.json"))
        else:
            prs = json.loads(run(["gh", "pr", "list", "--repo", REPO, "--state", "open", "--search",
                                  "t27b", "--json",
                                  "number,title,body,headRefName,baseRefName,mergeable,statusCheckRollup"],
                                 timeout=90))
        return [p for p in prs if is_t27b(p)]

    def claim(self):
        text = self._fx("claim.json") if self.fixture else _read(os.path.join(self.state, "claim.json"))
        return json.loads(text) if text and text.strip() else None

    def alive(self, pid):
        if self.fixture:
            return str(pid) in self._fx("alive_pids.txt").split()
        try:
            os.kill(int(pid), 0)
            return True
        except ProcessLookupError:
            return False
        except PermissionError:
            return True

    def ledger(self):
        return self._fx("ledger.md") if self.fixture else _read(os.path.join(self.state, "ledger.md"))

    def worktrees(self):
        if self.fixture:
            return [p for p in self._fx("worktrees.txt").split("\n") if p.strip()]
        return sorted(p for p in glob.glob("/tmp/t27b-*")
                      if os.path.isdir(p) and os.path.exists(os.path.join(p, ".git")))

    def cwds(self):
        text = self._fx("cwd.txt") if self.fixture else run(["lsof", "-w", "-d", "cwd", "-Fpn"], timeout=60)
        return parse_lsof(text)

    def ps(self):
        return self._fx("ps.txt") if self.fixture else run(["ps", "-axo", "pid=,etime=,command="])

    def railway(self):
        if self.fixture:
            return self._fx("railway.txt").strip()
        return run(["railway", "--version"], timeout=20).strip()

    def _get(self, path):
        try:
            with urllib.request.urlopen(f"{self.lab}/{path}", timeout=60) as r:
                return r.read().decode("utf-8")
        except Exception as e:  # noqa: BLE001 -- any failure is "could not read"
            raise Unreadable(f"{self.lab}/{path}: {e}")

    def run_shas(self):
        if self.fixture:
            d = os.path.join(self.fixture, "runs")
            if not os.path.isdir(d):
                raise Unreadable("fixture runs/ absent")
            names = os.listdir(d)
        else:
            names = re.findall(r'href="([0-9a-f]{7,40}\.json)"', self._get("runs/"))
        return sorted(n[:-5] for n in names if n.endswith(".json"))

    def run_json(self, sha):
        text = self._fx(os.path.join("runs", sha + ".json")) if self.fixture else self._get(f"runs/{sha}.json")
        try:
            return json.loads(text)
        except ValueError as e:
            raise Unreadable(f"runs/{sha}.json: {e}")


def _read(path):
    try:
        with open(path, encoding="utf-8") as f:
            return f.read()
    except FileNotFoundError:
        return None
    except OSError as e:
        raise Unreadable(f"{path}: {e}")


def parse_time(s):
    s = s.strip().replace("Z", "+00:00")
    if re.match(r"^\d{4}-\d\d-\d\dT\d\d:\d\d\+", s):  # minute precision, as the ledger writes it
        s = s.replace("+", ":00+", 1)
    t = dt.datetime.fromisoformat(s)
    return t if t.tzinfo else t.replace(tzinfo=dt.timezone.utc)


def parse_lsof(text):
    """lsof -Fpn: a `p<pid>` line, then `n<path>` for its cwd. Returns [(pid, path)]."""
    out, pid = [], None
    for line in text.splitlines():
        if line.startswith("p"):
            pid = line[1:]
        elif line.startswith("n") and pid:
            out.append((pid, line[1:]))
    return out


def is_t27b(pr):
    """GitHub's search `t27b` also matches any PR whose body mentions it in passing
    (live, 2026-10-04: 16 hits, 2 of them t27b work). A t27b PR names t27b in its
    title or head branch, or refers to the epic #6063 or the umbrella #5977."""
    text = f"{pr.get('title', '')} {pr.get('headRefName', '')}".lower()
    return "t27b" in text or bool(re.search(r"#(6063|5977)\b", pr.get("body") or ""))


def required_states(pr):
    """{check name: conclusion-or-state} for the required checks that reported."""
    got = {}
    for c in pr.get("statusCheckRollup") or []:
        name = c.get("name") or c.get("context")
        if name in REQUIRED:
            got[name] = (c.get("conclusion") or c.get("state") or c.get("status") or "").upper()
    return got


def honest(lab):
    """The numbers a report may quote. `t27b_pass` alone is not one of them: it
    includes passes on specs the reference fails (#6184)."""
    s = lab.get("summary") or {}
    outside = s.get("t27b_pass_where_reference_does_not") or 0
    in_ref = s.get("t27b_pass_where_reference_passes")
    if in_ref is None and s.get("t27b_pass") is not None:
        in_ref = s["t27b_pass"] - outside
    h = {"in_ref": in_ref, "reference": s.get("reference_pass"), "outside": outside,
         "mismatch": s.get("mismatch"), "checked": None, "compile_only": None, "frontend": []}
    results = lab.get("results")
    if isinstance(results, list) and results:
        ok = [x for x in results if x.get("t27b") == "pass" and x.get("reference") == "pass"]
        h["checked"] = sum(1 for x in ok if (x.get("tests") or 0) + (x.get("invariants") or 0) > 0)
        h["compile_only"] = len(ok) - h["checked"]
        h["frontend"] = sorted(x.get("file", "?") for x in results
                               if x.get("t27b") == "frontend" and x.get("reference") == "pass")
    return h


def worktree_state(path):
    git_path = lambda p: run(["git", "rev-parse", "--git-path", p], cwd=path).strip()  # noqa: E731
    midop = []
    for marker, label in (("MERGE_HEAD", "merge"), ("rebase-merge", "rebase"),
                          ("rebase-apply", "rebase"), ("CHERRY_PICK_HEAD", "cherry-pick")):
        p = git_path(marker)
        if not os.path.isabs(p):
            p = os.path.join(path, p)
        if os.path.exists(p):
            midop.append(label)
    dirty = [l for l in run(["git", "status", "--porcelain", "--untracked-files=normal"],
                            cwd=path).splitlines() if l.strip()]
    branch = run(["git", "rev-parse", "--abbrev-ref", "HEAD"], cwd=path).strip()
    index = os.path.join(path, git_path("index")) if not os.path.isabs(git_path("index")) else git_path("index")
    mtime = os.path.getmtime(index) if os.path.exists(index) else None
    return {"path": path, "branch": branch, "midop": sorted(set(midop)), "dirty": len(dirty),
            "index_mtime": mtime}


def collect(src, args):
    """Read every source once. Each value is the data or an Unreadable."""
    data = {}
    for key, fn in (("now", src.now), ("lab", src.lab_json), ("master", src.master), ("prs", src.prs),
                    ("claim", src.claim), ("ledger", src.ledger), ("worktrees", src.worktrees),
                    ("cwds", src.cwds), ("ps", src.ps), ("railway", src.railway)):
        try:
            data[key] = fn()
        except Unreadable as e:
            data[key] = e
        except (ValueError, KeyError) as e:
            data[key] = Unreadable(f"{key}: {e}")
    if isinstance(data["now"], Unreadable):
        data["now"] = dt.datetime.now(dt.timezone.utc)
    wts = []
    if not isinstance(data["worktrees"], Unreadable):
        for p in data["worktrees"]:
            try:
                wts.append(worktree_state(p))
            except Unreadable as e:
                wts.append({"path": p, "error": str(e)})
    data["wt"] = wts
    claim = data["claim"]
    data["claim_alive"] = None
    if isinstance(claim, dict) and claim.get("pid") is not None:
        try:
            data["claim_alive"] = src.alive(claim["pid"])
        except Unreadable as e:
            data["claim_alive"] = e
    return data


def anomalies(d, args):
    out = []
    add = lambda code, what, fix: out.append({"code": code, "what": what, "fix": fix})  # noqa: E731
    now = d["now"]

    lab = d["lab"]
    if isinstance(lab, Unreadable):
        add("LAB-UNREADABLE", str(lab), "open the lab URL; check the Railway deploy (railway 5.x: logs -s t27b-lab)")
    else:
        ref, commit = lab.get("ref"), lab.get("commit", "")
        if ref != "master":
            add("LAB-NOT-MASTER", f"lab ref is {ref!r}", "its numbers are that branch's; after the lab PR merges set T27_REF=master")
        master = d["master"]
        if isinstance(master, str) and master and commit and commit != master:
            add("LAB-BEHIND", f"lab commit {commit[:9]}, master {master[:9]}", "wait for the lab's next run before quoting master numbers")
        fin = lab.get("finished")
        if fin:
            age = (now - parse_time(fin)).total_seconds() / 3600
            if age > args.stale_hours:
                add("LAB-STALE", f"last run finished {age:.1f} h ago", "check /status.json and the Railway deploy logs")
        s = lab.get("summary") or {}
        if s.get("mismatch", 0):
            add("LAB-MISMATCH", f"mismatch {s['mismatch']}", "stop the lanes; reduce and report each mismatch (runs/<sha>.json)")
        if s.get("t27b_pass_where_reference_does_not", 0):
            add("LAB-OUTSIDE-REF", f"t27b passes {s['t27b_pass_where_reference_does_not']} spec(s) the reference fails",
                "list them; a reference defect or a vacuous t27b pass, not coverage")
        errs = {k: s.get(k, 0) for k in ("reference_lab_error", "crash", "timeout", "t27b_fail") if s.get(k, 0)}
        if errs:
            add("LAB-ERROR", ", ".join(f"{k} {v}" for k, v in errs.items()), "read runs/<sha>.log for each")
        for f in honest(lab)["frontend"]:
            add("LAB-FRONTEND-DISAGREES", f"{f}: t27b's frontend rejects it, the reference passes it",
                "a t27b parser/typecheck defect (or a reference that accepts too much): file it, not a blocker")
        steps = lab.get("steps") or {}
        red = [k for k, v in steps.items() if isinstance(v, dict) and v.get("ok") is False]
        ct = steps.get("cargo_test_t27b") or {}
        if ct.get("failed", 0):
            red.append(f"cargo_test_t27b failed {ct['failed']}")
        if red:
            add("LAB-TESTS-RED", "; ".join(red), "the lab's run is not a measurement until these are green")

    prs = d["prs"]
    if isinstance(prs, Unreadable):
        add("PR-UNREADABLE", str(prs), "gh auth status; retry")
    else:
        for pr in prs:
            n = pr.get("number")
            if pr.get("mergeable") == "CONFLICTING":
                add("PR-CONFLICTING", f"#{n} {pr.get('headRefName')}",
                    "one subagent merges origin/master into it (merge commit, plain push), in its own fresh worktree")
            bad = {k: v for k, v in required_states(pr).items() if v in ("FAILURE", "ERROR", "CANCELLED", "TIMED_OUT")}
            if bad:
                add("PR-REQUIRED-RED", f"#{n} " + ", ".join(f"{k}={v}" for k, v in sorted(bad.items())),
                    "read the failing job log; fix on the branch")
            if pr.get("baseRefName") not in (None, "master"):
                add("PR-BASE-NOT-MASTER", f"#{n} base {pr.get('baseRefName')}",
                    "when the parent merges: gh pr edit --base master, then wait for checks")

    claim, alive = d["claim"], d["claim_alive"]
    if isinstance(claim, dict) and not claim.get("released"):
        since = claim.get("since")
        age = (now - parse_time(since)).total_seconds() / 60 if since else None
        if alive is False:
            add("CLAIM-DEAD", f"pid {claim.get('pid')} not running, claim since {since}",
                "the claim is free; check the worktrees below before reusing any of them")
        elif alive is True and age is not None and age > args.claim_minutes:
            add("CLAIM-OLD", f"pid {claim.get('pid')} alive, claim {age:.0f} min old",
                "a tick is running long; read uptime and its processes before calling it stuck")

    cwds = d["cwds"] if not isinstance(d["cwds"], Unreadable) else []
    for w in d["wt"]:
        if "error" in w:
            continue
        owners = sorted({pid for pid, p in cwds if p == w["path"] or p.startswith(w["path"] + "/")}, key=int)
        who = f"processes inside: {', '.join(owners)}" if owners else "no process inside"
        age = ""
        if w.get("index_mtime"):
            mins = (now.timestamp() - w["index_mtime"]) / 60
            age = f", index touched {mins:.0f} min ago"
        if w["midop"]:
            add("WORKTREE-MIDOP", f"{w['path']} ({w['branch']}) mid-{'/'.join(w['midop'])}; {who}{age}",
                "someone's work if a process is inside or the index is recent: never reuse, reset or abort it; "
                "start new work in a fresh /tmp/t27b-<lane>-<UTC> path")
        elif w["dirty"]:
            add("WORKTREE-DIRTY", f"{w['path']} ({w['branch']}) {w['dirty']} uncommitted path(s); {who}{age}",
                "same as above: read it, do not clean it")

    rw = d["railway"]
    if not isinstance(rw, Unreadable):
        m = re.search(r"(\d+)\.(\d+)\.(\d+)", rw)
        if m and int(m.group(1)) < 5:
            add("RAILWAY-OLD-CLI", f"first railway on PATH is {m.group(0)}",
                "call ~/.nvm/versions/node/v22.22.0/bin/railway or ~/.bun/bin/railway (5.x)")

    ps = d["ps"]
    if not isinstance(ps, Unreadable) and not isinstance(lab, Unreadable):
        for line in ps.splitlines():
            if re.search(r"\bt27b\b.*\bcorpus\b.*--reference\b", line) and "tri t27b" not in line:
                parts = line.split(None, 2)
                add("LOCAL-REFERENCE", f"pid {parts[0]} running {parts[1]}: {parts[2][:100]}",
                    "the lab computes the reference denominator; ask the owner of that run before stopping it")

    led = d["ledger"]
    if isinstance(led, str):
        rows = [l for l in led.splitlines() if l.startswith("| 20")]
        if rows:
            last = parse_time(rows[-1].split("|")[1])
            hrs = (now - last).total_seconds() / 3600
            if hrs > args.quiet_hours:
                add("LEDGER-QUIET", f"last ledger row {hrs:.1f} h old", "is the scheduled task t27b-queen-steward still enabled?")
    return out


def lab_card(lab):
    if isinstance(lab, Unreadable):
        return [f"lab        UNREADABLE: {lab}"]
    s = lab.get("summary") or {}
    h = honest(lab)
    pct = f" ({100.0 * h['in_ref'] / h['reference']:.1f}%)" if h["in_ref"] is not None and h["reference"] else ""
    lines = [f"lab        ref {lab.get('ref')} @ {str(lab.get('commit', ''))[:9]}, finished {lab.get('finished')}",
             f"           t27b passes {h['in_ref']} of the {h['reference']} specs the reference passes{pct}, "
             f"mismatch {h['mismatch']}  ({s.get('files')} files)"]
    if h["checked"] is not None:
        lines.append(f"           of those {h['checked']} ran a test or invariant, {h['compile_only']} compile-only")
    lines.append(f"           not counted: {h['outside']} t27b pass(es) where the reference fails, "
                 f"{len(h['frontend'])} frontend reject(s) where it passes")
    for b in (lab.get("top_blockers") or [])[:5]:
        lines.append(f"           blocker {b.get('construct'):<34} first {b.get('first'):>4}  all {b.get('all')}")
    return lines


def status(d):
    lines = lab_card(d["lab"])
    m = d["master"]
    lines.append(f"master     {m[:9] if isinstance(m, str) else 'UNREADABLE: ' + str(m)}")
    prs = d["prs"]
    if isinstance(prs, Unreadable):
        lines.append(f"prs        UNREADABLE: {prs}")
    else:
        for pr in prs:
            req = required_states(pr)
            reqs = " ".join(f"{k}={req.get(k, 'none')}" for k in REQUIRED)
            lines.append(f"pr         #{pr['number']} {pr.get('mergeable')} base={pr.get('baseRefName')} {reqs}  {pr.get('title', '')[:60]}")
        if not prs:
            lines.append("prs        none open matching t27b")
    c = d["claim"]
    if isinstance(c, dict):
        lines.append(f"claim      since {c.get('since')} pid {c.get('pid')} alive={d['claim_alive']}"
                     + (" released" if c.get("released") else ""))
    else:
        lines.append(f"claim      {'none' if c is None else c}")
    led = d["ledger"]
    if isinstance(led, str):
        rows = [l for l in led.splitlines() if l.startswith("| 20")]
        lines.append(f"ledger     {rows[-1][:160] if rows else 'no rows'}")
    for w in d["wt"]:
        if "error" in w:
            lines.append(f"worktree   {w['path']} UNREADABLE: {w['error']}")
        else:
            lines.append(f"worktree   {w['path']} {w['branch']} dirty={w['dirty']} midop={','.join(w['midop']) or '-'}")
    return lines


def delta(src, frm, to):
    """Per-spec transitions between two lab runs. Returns (findings, header)."""
    a, b = src.run_json(frm), (src.lab_json() if to == "latest" else src.run_json(to))
    by = lambda run: {x["file"]: x for x in run.get("results") or [] if "file" in x}  # noqa: E731
    ra, rb = by(a), by(b)
    if not ra or not rb:
        raise Unreadable("a run has no per-spec results")
    out = []
    add = lambda code, f, what: out.append({"code": code, "file": f, "what": what})  # noqa: E731
    checks = lambda x: (x.get("tests") or 0) + (x.get("invariants") or 0)  # noqa: E731
    bad = ("fail", "mismatch", "crash")
    for f in sorted(set(ra) | set(rb)):
        x, y = ra.get(f), rb.get(f)
        if x is None or y is None:
            continue
        if x.get("reference") != y.get("reference"):
            add("REF-MOVED", f, f"reference {x.get('reference')} -> {y.get('reference')}")
            continue
        if y.get("reference") != "pass":
            continue
        tx, ty = x.get("t27b"), y.get("t27b")
        if tx == "pass" and ty != "pass":
            add("REGRESSED", f, f"t27b pass -> {ty}: {(y.get('detail') or '')[:100]}")
        elif ty in bad and tx not in bad:
            add("NEW-MISMATCH", f, f"t27b {tx} -> {ty}: {(y.get('detail') or '')[:100]}")
        elif tx != "pass" and ty == "pass":
            add("GAINED", f, f"t27b {tx} -> pass" + ("" if checks(y) else " (compile-only)"))
        elif tx == ty == "pass" and checks(x) and not checks(y):
            add("CHECK-LOST", f, f"tests+invariants {checks(x)} -> 0")
    ha, hb = honest(a), honest(b)
    head = (f"from {str(a.get('commit', ''))[:9]} ({a.get('ref')}, {a.get('finished')})  "
            f"to {str(b.get('commit', ''))[:9]} ({b.get('ref')}, {b.get('finished')})\n"
            f"in-reference passes {ha['in_ref']}/{ha['reference']} -> {hb['in_ref']}/{hb['reference']}; "
            f"checked {ha['checked']} -> {hb['checked']}")
    return out, head


def previous_run(src, to):
    """The latest run that finished before --to, by `finished` (the listing is by sha)."""
    target = src.lab_json() if to == "latest" else src.run_json(to)
    tfin, tsha = target.get("finished") or "", target.get("commit")
    best = None
    for sha in src.run_shas():
        if tsha and (sha == tsha or tsha.startswith(sha) or sha.startswith(tsha)):
            continue
        fin = src.run_json(sha).get("finished") or ""
        if fin < tfin and (best is None or fin > best[0]):
            best = (fin, sha)
    if best is None:
        raise Unreadable("no earlier lab run to compare with")
    return best[1]


DELTA_RED = ("REGRESSED", "NEW-MISMATCH", "CHECK-LOST")


def main_delta(args):
    src = Sources(fixture=args.fixture, lab=args.lab)
    try:
        frm = args.from_ or previous_run(src, args.to)
        found, head = delta(src, frm, args.to)
    except Unreadable as e:
        print(f"tri t27b delta: could not read: {e}", file=sys.stderr)
        return 2
    if args.json:
        print(json.dumps({"header": head, "findings": found}, indent=1))
    else:
        print(head)
        for f in found:
            print(f"{f['code']:<13} {f['file']}  {f['what']}")
        red = sum(1 for f in found if f["code"] in DELTA_RED)
        print(f"tri t27b delta: {red} regression(s), {sum(1 for f in found if f['code'] == 'GAINED')} gained, "
              f"{sum(1 for f in found if f['code'] == 'REF-MOVED')} reference move(s)")
    return 1 if any(f["code"] in DELTA_RED for f in found) else 0


def main(argv):
    ap = argparse.ArgumentParser(prog="tri t27b", description=__doc__.split("\n")[0])
    ap.add_argument("action", choices=("status", "doctor", "delta"))
    ap.add_argument("--from", dest="from_", help="delta: the earlier lab run's sha (default: the run before --to)")
    ap.add_argument("--to", default="latest", help="delta: the later run's sha (default: latest.json)")
    ap.add_argument("--fixture", help="read every source from this directory (tests)")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--lab", default=LAB)
    ap.add_argument("--stale-hours", type=float, default=6.0)
    ap.add_argument("--claim-minutes", type=float, default=100.0)
    ap.add_argument("--quiet-hours", type=float, default=2.5)
    try:
        args = ap.parse_args(argv)
    except SystemExit as e:
        return 2 if e.code else 0
    if args.action == "delta":
        return main_delta(args)
    src = Sources(fixture=args.fixture, lab=args.lab)
    d = collect(src, args)
    found = anomalies(d, args)
    if args.action == "status":
        if args.json:
            lab = d["lab"]
            print(json.dumps({"anomalies": found,
                              "lab": None if isinstance(lab, Unreadable) else lab.get("summary"),
                              "honest": None if isinstance(lab, Unreadable) else honest(lab)}, indent=1))
        else:
            print("\n".join(status(d)))
            print(f"anomalies  {len(found)} (tri t27b doctor)")
        return 0
    if args.json:
        print(json.dumps(found, indent=1))
    else:
        for a in found:
            print(f"{a['code']:<20} {a['what']}\n{'':<20} fix: {a['fix']}")
        print(f"tri t27b doctor: {len(found)} anomaly(ies)")
    return 1 if found else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
