# NOW -- Rust value parameter bindings retain mutation and copy semantics (2026-10-04)

## Candidate compiler ring (Closes #5901, Refs #5910)

- Rust generation marks a by-value scalar or fixed-array binding mutable only when its body writes it. Local and loop-capture shadows do not mutate the outer argument. Parameter types and reference paths remain unchanged.
- The new source fixture has eight functions and nine executable assertions. Real Rust generation, compilation and execution verify 256 scalar inputs with 16 array/counter cases, caller copy semantics, existing reference writes, a type-correct negative mutation and local scopes. A fourth generation-only check covers collection-capture scope without claiming loop-body execution.
- The actual tri-net Rust corpus improves from89/102 to96/102 with no newly failing modules. Final generation matches the executed proof after the same Rust formatter; C/Zig fixture output remains unchanged and its nine checks per backend passed.
- Release build, parse, typecheck, native seal save/verify and the existing95-entry corpus ratchet pass. No ledger, CI exclusion, compiler pin or warning gate is weakened.
- Full no-fail-fast bootstrap Cargo reports2794 passed,1 failed,2 ignored. The remaining Lean classifier test also fails on the unchanged baseline for phi_rope/sacred_attention; #5910 tracks it. This candidate remains draft until M3 passes and is not a completed GOLD ring.
- FROZEN_HASH records the deliberate candidate compiler digest required by the build guard; this does not claim all tests are green. No radio or model inference is claimed.
