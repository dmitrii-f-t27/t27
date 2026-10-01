# NOW -- string == is the emitter's job: two string names compare with std.mem.eql (2026-10-01)

## bootstrap/src/compiler.rs -- the Zig backend knows which names are strings

- Zig has no `==` for `[]const u8`. The emitter already lowered a comparison
  to `std.mem.eql(u8, a, b)` when one side was a string literal, a `str`
  parameter or a `str` struct field. It missed what the automation specs
  write: two module-level string constants, or a `str` parameter against one
  (`if source == SOURCE_MEETING`, `assert(DIGEST_DOOR != TOOL)`). So
  `t27c test-report` reported those specs BLOCKED and ran none of their tests.
- Which names are strings comes only from DECLARATIONS, with no type
  inference. A name counts if it is one of:
  - a module const declared `: str`, or untyped and initialised by a string
    literal;
  - a local declared `str`, or initialised by a string literal, inside the
    function or test that declares it;
  - a call to a function this spec declares `-> str`.
- Locals leave the set when their function ends, so a numeric `want` in the
  next function keeps its plain `==`. Numeric comparisons do not change.
- `test-report`, before -> after:
  - specs/automation/ball-board.t27 on master (v5): BLOCKED -> 17/17 pass;
    the feat/ball-board-v2 copy: BLOCKED -> 16/16.
  - the copy on master at a3472e383 (v1): BLOCKED -> 9/9.
  - specs/automation/mail-push.t27: BLOCKED -> 11/11.
  - specs/port/tools/run_conformance_vvp.t27: BLOCKED -> 6/6.
  - specs/trinity/compiler_matrix.t27: BLOCKED -> 6/6.
- Corpus: the Zig output changes for 10 of the 1124 specs. Under
  `zig test --test-no-exec`:
  - 4 of them go from FAIL to OK;
  - the other 6 still fail on their first error, which is unrelated and the
    same as before;
  - no error appears that was not there before.
- Ratchet (W628): the same verdict before and after. Ledger 151/152, 67
  unexpected failures and 57 unexpected passes. Master is already red there,
  for parse, typecheck and Verilog reasons this change does not touch.
- Seals: the FROZEN seal moves (FROZEN.md section 5), as it did in #3962,
  #3973 and #4114. Four spec seals now record the old, invalid Zig. They cover
  specs/account/auth.t27 and specs/github/tests/e2e_full_flow.t27, and in both
  a literal-initialised local is now compared with `std.mem.eql`.
  `check_seal_currency` goes from 592 to 596 stale; master is already red
  there. They are not resealed here.
- Regression guard: bootstrap/tests/string_eq_zig.rs. It covers the literal,
  const, param, local and `-> str` call cases, a numeric control, a no-leak
  case, and a `test-report` run end to end (6/6).
