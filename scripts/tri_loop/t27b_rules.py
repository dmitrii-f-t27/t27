"""The t27b steward's decisions, loaded from their t27 spec (#6198).

`specs/tri/t27b/steward.t27` decides what a lab run means: which per-spec
transition is a regression, the honest percentage, and what each ledger
entry's ratchet finding is. This module holds no
decision of its own. It compiles `gen/c/tri/t27b/steward.c` (written by
`t27c gen-c` from that spec, never by hand -- L2) with the system C compiler
into a cache and calls it through ctypes.

What stays here is plumbing: the lab's JSON verdict strings map to the
spec's verdict codes. An unknown string is an error, never a guess.
"""

import ctypes
import hashlib
import os
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SPEC = ROOT / "specs" / "tri" / "t27b" / "steward.t27"
GEN = ROOT / "gen" / "c" / "tri" / "t27b" / "steward.c"

# The lab's verdict strings, in the order of the spec's verdict codes.
VERDICTS = ("pass", "pass_vacuous", "blocked", "frontend", "codegen",
            "fail", "mismatch", "crash", "timeout", "not run", "missing", "lab_error")
DELTAS = (None, "REF-MOVED", "REGRESSED", "NEW-MISMATCH", "GAINED", "CHECK-LOST")
RATCHETS = (None, "VACUITY MEASURED", "UNEXPECTED FAILURE", "UNEXPECTED PASS", "MOVED",
            "STALE", "UNJUDGED", "UNLISTED", "OVER CAP", "BAD REASON")
REASONS = (None, "unimplemented", "reference-bug", "n/a")
CLAIMS = (None, "CLAIM-DEAD", "CLAIM-OLD")
CHECKOUTS = (None, "LAB-CHECKOUT", "LAB-RECLONED")


class RulesUnavailable(RuntimeError):
    """The spec's compiled rules could not be loaded; nothing is decided without them."""


def _cache_dir():
    base = os.environ.get("XDG_CACHE_HOME") or os.path.join(os.path.expanduser("~"), ".cache")
    return Path(base) / "t27" / "t27b-rules"


def _build():
    try:
        src = GEN.read_bytes()
    except OSError as e:
        raise RulesUnavailable(f"{GEN.relative_to(ROOT)}: {e}") from None
    lib = _cache_dir() / f"steward-{hashlib.sha256(src).hexdigest()[:16]}.{'dylib' if sys.platform == 'darwin' else 'so'}"
    if not lib.exists():
        lib.parent.mkdir(parents=True, exist_ok=True)
        cc = os.environ.get("CC", "cc")
        fd, tmp = tempfile.mkstemp(dir=lib.parent, suffix=lib.suffix)
        os.close(fd)
        p = subprocess.run([cc, "-shared", "-fPIC", "-O2", "-w", "-o", tmp, str(GEN)],
                           capture_output=True, text=True)
        if p.returncode != 0:
            os.unlink(tmp)
            raise RulesUnavailable(f"{cc} could not compile {GEN.relative_to(ROOT)}: {p.stderr.strip()[:300]}")
        os.replace(tmp, lib)
    so = ctypes.CDLL(str(lib))
    so.delta_code.argtypes = [ctypes.c_uint8] * 4 + [ctypes.c_uint32] * 2
    so.delta_code.restype = ctypes.c_uint8
    so.delta_is_red.argtypes = [ctypes.c_uint8]
    so.delta_is_red.restype = ctypes.c_bool
    so.pct_tenths.argtypes = [ctypes.c_uint32, ctypes.c_uint32]
    so.pct_tenths.restype = ctypes.c_uint32
    so.is_pass.argtypes = [ctypes.c_uint8]
    so.is_pass.restype = ctypes.c_bool
    so.entry_code.argtypes = [ctypes.c_uint8] * 3 + [ctypes.c_bool] * 2
    so.entry_code.restype = ctypes.c_uint8
    so.unlisted.argtypes = [ctypes.c_uint8, ctypes.c_bool]
    so.unlisted.restype = ctypes.c_bool
    so.over_cap.argtypes = [ctypes.c_uint32, ctypes.c_uint32, ctypes.c_bool]
    so.over_cap.restype = ctypes.c_bool
    so.ratchet_is_red.argtypes = [ctypes.c_uint8]
    so.ratchet_is_red.restype = ctypes.c_bool
    so.bless_reason.argtypes = [ctypes.c_uint8, ctypes.c_uint8]
    so.bless_reason.restype = ctypes.c_uint8
    so.cap_rises.argtypes = [ctypes.c_uint32, ctypes.c_uint32, ctypes.c_bool]
    so.cap_rises.restype = ctypes.c_bool
    for name in ("lab_stale", "ledger_quiet"):
        getattr(so, name).argtypes = [ctypes.c_uint32, ctypes.c_uint32]
        getattr(so, name).restype = ctypes.c_bool
    so.claim_code.argtypes = [ctypes.c_uint8, ctypes.c_uint32, ctypes.c_uint32]
    so.claim_code.restype = ctypes.c_uint8
    so.railway_old.argtypes = [ctypes.c_uint32]
    so.railway_old.restype = ctypes.c_bool
    so.checkout_code.argtypes = [ctypes.c_bool, ctypes.c_bool]
    so.checkout_code.restype = ctypes.c_uint8
    so.cap_rise_is_new.argtypes = [ctypes.c_uint32] * 3
    so.cap_rise_is_new.restype = ctypes.c_bool
    so.is_alarm.argtypes = [ctypes.c_uint8, ctypes.c_uint8]
    so.is_alarm.restype = ctypes.c_bool
    return so


_lib = None


def lib():
    global _lib
    if _lib is None:
        _lib = _build()
    return _lib


def verdict(v):
    if v not in VERDICTS:
        raise RulesUnavailable(f"unknown lab verdict {v!r}; add it to specs/tri/t27b/steward.t27 first")
    return VERDICTS.index(v)


def delta(ref_a, ref_b, t_a, t_b, checks_a, checks_b):
    """The spec's transition name for one file between two runs, or None."""
    return DELTAS[lib().delta_code(verdict(ref_a), verdict(ref_b), verdict(t_a), verdict(t_b),
                                   checks_a, checks_b)]


def is_red(name):
    return bool(lib().delta_is_red(DELTAS.index(name)))


def pct(in_ref, reference):
    """The honest percentage as text, one decimal, as the spec rounds it."""
    t = lib().pct_tenths(in_ref, reference)
    return f"{t // 10}.{t % 10}"


def is_pass(v):
    return bool(lib().is_pass(verdict(v)))


def entry(want, reference, got, counted, same_blocker):
    """The spec's ratchet finding for one ledger entry against one run record, or None."""
    return RATCHETS[lib().entry_code(verdict(want), verdict(reference), verdict(got),
                                     bool(counted), bool(same_blocker))]


def unlisted(reference, listed):
    return bool(lib().unlisted(verdict(reference), bool(listed)))


def over_cap(not_pass, cap):
    """A cap that is not a whole number is no cap; the spec calls that over."""
    has = isinstance(cap, int) and not isinstance(cap, bool) and cap >= 0
    return bool(lib().over_cap(not_pass, cap if has else 0, has))


def ratchet_is_red(kind):
    return bool(lib().ratchet_is_red(RATCHETS.index(kind)))


def bless_reason(got, kept):
    """The reason bless writes for a non-pass entry, or None: bless refuses it.
    `kept` is the reason already in the ledger; one that is not a known reason is no reason."""
    return REASONS[lib().bless_reason(verdict(got), REASONS.index(kept) if kept in REASONS else 0)]


def cap_rises(not_pass, old_cap):
    has = isinstance(old_cap, int) and not isinstance(old_cap, bool) and old_cap >= 0
    return bool(lib().cap_rises(not_pass, old_cap if has else 0, has))


def _minutes(x):
    """Whole minutes for the spec's u32 thresholds; a negative age (clock skew) is 0."""
    return max(0, min(int(x), 2**32 - 1))


def lab_stale(age_min, limit_min):
    return bool(lib().lab_stale(_minutes(age_min), _minutes(limit_min)))


def ledger_quiet(age_min, limit_min):
    return bool(lib().ledger_quiet(_minutes(age_min), _minutes(limit_min)))


def claim(alive, age_min, limit_min):
    """The doctor's claim finding, or None. `alive` is True, False, or None (could not tell)."""
    code = 0 if alive is False else 1 if alive is True else 2
    return CLAIMS[lib().claim_code(code, _minutes(age_min or 0), _minutes(limit_min))]


def railway_old(major):
    return bool(lib().railway_old(_minutes(major)))


def checkout(ok, recloned):
    return CHECKOUTS[lib().checkout_code(bool(ok), bool(recloned))]


def cap_rise_is_new(not_pass, old_cap, new_not_pass):
    """True when the rise over `old_cap` is no larger than the specs new to the ledger."""
    return bool(lib().cap_rise_is_new(_minutes(not_pass), _minutes(old_cap), _minutes(new_not_pass)))


def is_alarm(t27b, reference):
    """True when a run record is a lab alarm: a mismatch or crash always, a fail or
    timeout only where the reference passes."""
    return bool(lib().is_alarm(verdict(t27b), verdict(reference)))
