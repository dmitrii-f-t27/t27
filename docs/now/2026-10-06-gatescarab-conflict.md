# NOW -- GateScarab declared twice: rename the SR-03 port's type (2026-10-06)

- Closes #6611. Master's Corpus ratchet is red with `+ GateScarab NEW conflict` after #6238 merged alongside the SR-04 port.
- `specs/port/trios/crates/trios-scarab-types/rings/SR-03/src/lib.t27`: `GateScarab` -> `GateScarabFour` (type and its functions). The rendered strings keep the Rust original's text.
- No ledger move, no seal touched.
