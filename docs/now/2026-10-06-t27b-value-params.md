# NOW -- t27b: array and struct parameters the body assigns (2026-10-06)

## value parameters (Closes #6569)

- New conformance spec `specs/tri/t27b/conformance/value_params.t27`. It gives 6 pass under `t27c test-report`, and the same 6 under t27b.
- An array or struct parameter the reference makes a `var` (`var p = p_arg;`, by t27c's own `collect_mutable_names` test) is copied into a slot of the callee at entry, and the body writes that copy. The caller's value never changes, as in the reference.
- A parameter written only deeper (`p.f[i] = ...`) stays a constant in the reference, which refuses the write; t27b still refuses it.
- Code is in the new submodule `cli/t27b/src/lower/paramcopy.rs`, with one hook in `lower.rs`. Tests are in `cli/t27b/tests/valueparams.rs`.
