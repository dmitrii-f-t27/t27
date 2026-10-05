# NOW -- catalog Broken in gHashTag/t27: five malformed sources fixed (2026-10-05)

## Five specs the parser rejected now pass every backend (Closes #6437)

- Re-measured the 59 t27 paths the SPECS catalog calls Broken with master t27c on the Railway lab (parse plus 7 backends): 24 are negative fixtures under `bootstrap/tests/`, 23 already pass on master, 7 fail only gen-verilog on `[N][]const u8` (#6438), 5 were malformed source.
- `examples/fpga/qmtech_minimal/design.t27` was `#`-comment pseudo-config; it is now module `QmtechMinimalDesign` (heartbeat pattern, UART timing, resource budget; 6 tests pass under zig, 3 invariants) and defers pins to `specs/boards/` and `fpga/HARDWARE_SSOT.md`.
- `specs/ar/datalog_engine.t27` (`loop`/`break` -> `while`), `specs/ar/ternary_logic.t27` (dropped `type Trit = Trit`), `specs/test_framework/graph_drift_detection.t27` and `verilog_bench_harness.t27` (`format!` -> `format`, tuple `for` and `if let` -> `keys()`/`contains_key`, `T?` -> `?T`); comment non-ASCII replaced (L3).
- Seals re-saved with `t27c seal --save` on the lab; the stale `qmtech_minimal_design.json` (module `design`) is replaced by `qmtech_minimal_QmtechMinimalDesign.json`.
