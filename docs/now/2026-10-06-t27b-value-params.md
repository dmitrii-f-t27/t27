# NOW -- t27b: arrays and structs the body assigns (2026-10-06)

## value parameters and locals (Closes #6569)

- New conformance spec `specs/tri/t27b/conformance/value_params.t27`. It gives 10 pass under `t27c test-report`, and the same 10 under t27b.
- An array or struct parameter that the reference makes a `var` (`var p = p_arg;`) is copied into a slot of the callee at entry, and the body writes that copy. The reference decides this with t27c's own `collect_mutable_names` test, and the caller's value never changes, as in the reference.
- An array or struct local that the same test names (`const` or `let`, typed or not) is a writable slot of its own, as the reference's `var` is.
- A name written only deeper (`p.f[i] = ...`, or a write inside a block the test does not walk) stays a constant in the reference, which refuses the write. t27b still refuses it too.
- Code is in the new submodule `cli/t27b/src/lower/refvars.rs`, with small hooks in `lower.rs`. Tests are in `cli/t27b/tests/valueparams.rs`. One case in `tests/source.rs` that expected `const p = Pt{..}; p.x = 3;` to be refused is dropped, because the reference accepts it.
