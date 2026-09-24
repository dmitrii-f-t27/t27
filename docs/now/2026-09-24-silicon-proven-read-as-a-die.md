# NOW -- "silicon-proven" read as a die that was never made (2026-09-24)

## GF-T results on the AX7203 now say FPGA-proven

- Thirteen lines in six files called the GF-T16 MAC and the trees built from it "silicon-proven", "ON SILICON" or "proven on live silicon". The evidence behind every one of them is an Artix-7 board (AX7203, gft_dot2 3/3); no die was ever fabricated. `STATUS.md:33` reserves SILICON for a received die, and `STATUS.md:112` forbids a SILICON claim anywhere in t27.
- Reworded to "FPGA-proven (AX7203)" in `bootstrap/tests/gft_dot2.rs`, `gft_dot4.rs`, `gft_dot8.rs`, `gft_layer2.rs` (comments, and one assert failure message that no test compares), the section 3 heading of `docs/GFT_WHITEPAPER.md`, and T86 / T88 of `docs/theory/IGLA-FORMAL-RESULTS.md`. What was measured, and on which board, is unchanged.
- Left alone on purpose: the trinity-fpga RTL embedded verbatim in those four tests (a copy -- the wording belongs to its source), and the six sealed specs `gft_dot2`, `gft_dot4`, `gft_dot8`, `gft_layer2`, `gft_mul_rne`, `tnf17`. Their seals hash the spec text, so a comment edit there is a reseal, not a wording fix.
- Test function names such as `spec_first_gft_mac_matches_silicon_proven_rtl` are unchanged too: renaming a test moves every filter and baseline that names it.
