# Wave Loop 884 — Issue TBD

**Branch:** `wave-loop-884` (to be created from `wave-loop-883` HEAD)
**Parent branch:** `wave-loop-883` HEAD
**Date:** 2026-08-06
**Issue:** TBD (create after W883 issue #1814 / PR #1815 lands)
**PR:** TBD (to open)
**Cooperation variant:** A (recommended)

## Goal

Select one of three W884 cooperation variants and close the wave with a green targeted
test, updated seals, and the standard close-out artifacts (report, evidence, next-wave
plan).

Close Wave Loop 884 by validating a module-scope `[587][2]^6 Pt` packed array-of-struct
variable initialized from a function call, with indexed signed field writes and `assert_eq`
read-back in a `bench` block. Earlier wave PRs (#1810 W881, #1813 W882, #1815 W883) remain
open awaiting review, so W884 will be branched from `wave-loop-883` HEAD to avoid blocking
the sequence.

## Acceptance criteria

- [ ] Generator `scripts/gen_w884.py` with `OUTER = 587`, `MID_IDX = 293`; copy hazard fixed before first run.
- [ ] Witness `specs/scratch/w884_bench_module_587x2p6_aos_var_call_write.t27` generated and parsed.
- [ ] `t27c icarus-lowerable`, `icarus-simulate`, `icarus-cocotb`, and `seal --save` all PASS.
- [ ] Integration test `accepts_w884_bench_module_587x2p6_aos_var_call_write` added to `bootstrap/tests/icarus_lowerable.rs`.
- [ ] Full `cargo test --release --test icarus_lowerable` passes at **344/0** (targeted test green; pre-existing classifier failure tracked separately).
- [ ] `bootstrap/stage0/FROZEN_HASH` unchanged.
- [ ] Closeout report, next-wave plan, skills, and persistent memory updated.
- [ ] Commit with `Closes #<W884-issue>`, push branch, open PR to `master`.

## Notes

- Shape: `[587][2]^6 Pt` where `Pt = pub struct Pt { x : i16, y : i16 }`.
- Total elements: `587 x 64 = 37,568`.
- Packed vector width: `37,568 x 32 = 1,202,176` bits (~1.147 MiBit).
- `MID_IDX = 293`; frame-condition element `[293][1][0][0][0][0][0]` is element
  `293*64 + 32 = 18,784`.
- Generator script: `scripts/gen_w884.py` (copy from `scripts/gen_w883.py`, set
  `OUTER = 587` and `MID_IDX = 293`, fix module prefix).
- Use `assert_eq` checks on changed elements (Icarus simulation path does not emit `assert_ne`).
- Include `make_grid(32768)` period-identity check because `32768 == 0 (mod 32768)`.
- Zero compiler / reference-model / `FROZEN_HASH` changes expected for the witness.

---

- **Variant A (recommended):** continue the odd outer-dimension ladder with `[587][2]^6 Pt`.
- **Variant B:** keep width at ~1.147 MiBit but move the packed var to bench/function scope.
- **Variant C:** add `if`-guarded indexed signed field writes at the current width.

---

phi^2 + 1/phi^2 = 3 | TRINITY
