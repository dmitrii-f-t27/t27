"""The t27b steward's decisions, loaded from their t27 spec (#6198).

`specs/tri/t27b/steward.t27` decides what a lab run means: which per-spec
transition is a regression, and the honest percentage. This module holds no
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
            "fail", "mismatch", "crash", "timeout", "not run")
DELTAS = (None, "REF-MOVED", "REGRESSED", "NEW-MISMATCH", "GAINED", "CHECK-LOST")


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
