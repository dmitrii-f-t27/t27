# NOW -- t27b: array literals by address, empty slice returns, unused tuple constants (2026-10-05)

## ExprArrayLiteral (Closes #6550)

- New conformance spec `specs/tri/t27b/conformance/array_literal.t27`. It passes `t27c test-report` (6 pass) and t27b (6 pass).
- `const xs = &[_]T{ ... }` (the reference's `&.{ ... }`) is lowered when every use is `xs.len` or an argument where the callee declares `[]const E`, with one E for all uses. It becomes an `[N]E` built once. Indexing such a local is still refused, because its elements are comptime values in the reference.
- `return [];` / `return []T{};` in a fn returning a slice is an empty slice.
- An untyped module constant bound to a list that no other node names is skipped, as Zig's lazy analysis skips it.
- Code is in the new submodule `cli/t27b/src/lower/arraylit.rs`, with three hooks in `lower.rs`. Tests are in `cli/t27b/tests/arraylit.rs`.
