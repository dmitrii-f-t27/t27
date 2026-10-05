//! End-to-end tests from t27 source text: the t27c front-end, lowering, then
//! every `test` and `invariant` block run in the reference interpreter and, on
//! arm64 macOS and arm64 Linux, in the JIT, which must agree with it.
//!
//! One test per construct family. Each checks both what runs (the outcome of
//! every block) and what is refused (the exact `unsupported construct`).

use std::path::Path;
use t27b::codegen::{self, TrapStyle};
use t27b::eval::{Interp, Stop};
use t27b::ir::*;
use t27b::jit::{Jit, JIT_SUPPORTED};
use t27b::{front, lower};

fn lower_src(src: &str) -> Result<Program, Vec<String>> {
    let parsed = front::parse(Path::new("/nonexistent/t.t27"), src).map_err(|e| vec![format!("parse: {}", e)])?;
    lower::lower_src(&parsed.ast, OverflowMode::Trap, Some(src)).map_err(|rs| rs.iter().map(|r| r.message()).collect())
}

/// Outcome of one block: `Ok(())`, or the trap kind and source line.
type Outcome = Result<(), (TrapKind, u32)>;

/// Run every test and invariant; returns (name, is_invariant, outcome).
fn run(src: &str) -> Vec<(String, bool, Outcome)> {
    let prog = lower_src(src).unwrap_or_else(|e| panic!("lowering failed:\n{}", e.join("\n")));
    let mut jit = if JIT_SUPPORTED {
        let code = codegen::compile(&prog, TrapStyle::Jit, true).expect("codegen");
        Some(Jit::load(&code, prog.funcs.len(), &prog.data, &prog.globals).expect("jit load"))
    } else {
        None
    };
    let mut out = Vec::new();
    for (id, f) in prog.tests() {
        let want = Interp::new(&prog).call(id, &[]);
        let o: Outcome = match &want {
            Ok(_) => Ok(()),
            Err(Stop::Trap { site, .. }) => {
                let s = &prog.sites[*site as usize];
                Err((s.kind, s.line))
            }
            Err(e) => panic!("{}: interpreter stopped: {:?}", f.name, e),
        };
        if let Some(j) = jit.as_mut() {
            let got = j.call(id as FuncId, &[]);
            match (&want, &got) {
                (Ok(_), Ok(_)) => {}
                (Err(Stop::Trap { site, .. }), Err(t)) => assert_eq!(*site, t.site, "{}: trap site", f.name),
                _ => panic!("{}: interpreter {:?}, jit {:?}", f.name, want, got),
            }
        }
        out.push((f.name.clone(), f.is_invariant, o));
    }
    out
}

/// The first rejection message, which must exist.
fn rejected(src: &str) -> String {
    match lower_src(src) {
        Ok(_) => panic!("expected a rejection"),
        Err(e) => e[0].clone(),
    }
}

fn names_ok(r: &[(String, bool, Outcome)]) -> Vec<(&str, bool, bool)> {
    r.iter().map(|(n, i, o)| (n.as_str(), *i, o.is_ok())).collect()
}

// ------------------------------------------------------------ invariants

#[test]
fn invariants_run_like_tests() {
    let src = "module inv;

const N: u32 = 4;

fn sq(x: u32) -> u32 {
    return x * x;
}

invariant n_positive
    assert N > 0

invariant sq_four: sq(2) == N;

invariant broken
    assert sq(3) == N

invariant overflow: sq(70000) > 0;

invariant all_id: forall x: u32, sq(x) >= 0;

test t1 {
    assert(sq(2) == 4);
}
";
    let r = run(src);
    assert_eq!(
        names_ok(&r),
        vec![
            ("n_positive", true, true),
            ("sq_four", true, true),
            ("broken", true, false),
            ("overflow", true, false),
            ("t1", false, true),
        ]
    );
    assert_eq!(r[2].2, Err((TrapKind::Assert, 15)));
    assert_eq!(r[3].2.unwrap_err().0, TrapKind::Overflow);
    // The quantified invariant has no body to run, and is not reported as run.
    let prog = lower_src(src).unwrap();
    assert_eq!(prog.unchecked, vec!["all_id".to_string()]);
}

/// `@compileAssert` is `assert`: t27c's Zig backend lowers both through one
/// arm. In an invariant (a `comptime` block there) a false one fails the
/// reference's compile and fails the invariant here; in a test it is a
/// runtime check in both. An integer `as f64` is `@floatFromInt` with result
/// type f64, which is what the reference emits for it.
#[test]
fn compile_assert_is_assert() {
    let src = "module ca;

const E: u8 = 3;
const N: u16 = 10;

fn half(x: u32) -> f64 {
    return x as f64 / 2.0;
}

invariant widths {
    @compileAssert(E + 7 == N);
}

invariant exponent_bounds {
    @compileAssert((E as f64 - 0.5) * 2.618033988749895 <= N as f64 - 1.0);
    @compileAssert(N as f64 - 1.0 <= (E as f64 + 0.5) * 2.618033988749895);
}

invariant broken {
    @compileAssert(E as f64 > 3.5, \"exponent too small\");
}

test runtime_operand {
    @compileAssert(half(7) == 3.5);
}

test runtime_false {
    @compileAssert(half(7) == 3.0);
}
";
    let r = run(src);
    assert_eq!(
        names_ok(&r),
        vec![
            ("widths", true, true),
            ("exponent_bounds", true, true),
            ("broken", true, false),
            ("runtime_operand", false, true),
            ("runtime_false", false, false),
        ]
    );
    assert_eq!(r[2].2, Err((TrapKind::Assert, 20)));
    assert_eq!(r[4].2, Err((TrapKind::Assert, 28)));

    // The message must be a string literal, as for `assert`.
    let m = rejected("module ca2;\nconst E: u8 = 3;\ninvariant i {\n    @compileAssert(E == 3, E);\n}\n");
    assert!(m.contains("unsupported construct ExprCall(assert with non-literal message)"), "{}", m);
}

#[test]
fn partially_parsed_invariant_is_rejected() {
    let src = "module inv;

const N: u32 = 4;

invariant q
    assert N > 0
    forall x in 0..N: x < N
";
    let m = rejected(src);
    assert!(m.starts_with("t27b: unsupported construct InvariantBlock at line 5"), "{}", m);
    assert!(m.contains("invariant `q` was only partially parsed"), "{}", m);
}

// --------------------------------------------------------------- structs

#[test]
fn structs_fields_pointers_and_results() {
    let src = "module st;

const Pt = struct {
    x: i32,
    y: i32,
    ok: bool,
};

const Box = struct { a: Pt, tag: u8, n: u64 = 7 };

const Node = struct { v: u32, next: *const Node };

const ORIGIN: Pt = Pt{ .x = -3, .y = 4, .ok = true };
const UNIT = Box{ .a = ORIGIN, .tag = 1 };
const OX: i32 = ORIGIN.x;

fn mk(x: i32, y: i32) Pt {
    return Pt{ .x = x, .y = y, .ok = false };
}

fn swap(p: Pt) Pt {
    return .{ .x = p.y, .y = p.x, .ok = p.ok };
}

fn sum(p: Pt) i32 {
    return p.x + p.y;
}

fn bump(p: *Pt) void {
    p.x += 1;
    p.*.y = 2;
}

fn incr(c: *u32) void {
    c.* += 1;
}

fn wrap(t: u8) Box {
    return Box{ .a = mk(5, 6), .tag = t };
}

test literals_and_fields {
    var p = mk(1, 2);
    p.x = 5;
    const q: Pt = .{ .x = 1, .y = 2, .ok = true };
    var b = Box{ .a = q, .tag = 3 };
    b.a.y = 9;
    bump(&p);
    assert(sum(p) == 8);
    assert(b.a.y == 9);
    assert(b.n == 7);
    assert(q.y == 2);
    assert(ORIGIN.x == -3);
    assert(OX == -3);
    assert(UNIT.a.y == 4 and UNIT.n == 7);
}

test results_and_copies {
    var p = mk(1, 2);
    p = swap(p);
    assert(p.x == 2 and p.y == 1);
    var w = wrap(9);
    assert(w.a.x == 5 and w.tag == 9);
    w.a = p;
    assert(w.a.y == 1);
    const c = w;
    w.tag = 0;
    assert(c.tag == 9);
    assert(sum(swap(mk(10, 20))) == 30);
    assert(mk(3, 4).y == 4);
    assert(sum(.{ .x = 1, .y = 1, .ok = true }) == 2);
}

test addresses {
    var n: u32 = 4;
    incr(&n);
    incr(&n);
    assert(n == 6);
    const tail = Node{ .v = 2, .next = undefined };
    const head = Node{ .v = 1, .next = &tail };
    assert(head.next.v == 2);
    var p = mk(0, 0);
    const pp = &p;
    pp.x = 7;
    pp.*.y += 3;
    assert(p.x == 7 and p.y == 3);
}

test overflow_through_a_field {
    var p = mk(2147483647, 0);
    p.x += 1;
}
";
    let r = run(src);
    assert_eq!(
        names_ok(&r),
        vec![
            ("literals_and_fields", false, true),
            ("results_and_copies", false, true),
            ("addresses", false, true),
            ("overflow_through_a_field", false, false),
        ]
    );
    assert_eq!(r[3].2.unwrap_err().0, TrapKind::Overflow);
    let prog = lower_src(src).unwrap();
    // mk, swap, sum, wrap take or return a struct; bump and incr take pointers.
    let internal: Vec<&str> = prog.internal_abi.iter().map(|&i| prog.funcs[i as usize].name.as_str()).collect();
    assert_eq!(internal, vec!["mk", "swap", "sum", "wrap"]);
}

/// A scalar field initialised from a call inside a struct literal is a store
/// of the call's value. It once went down the in-place path meant for an
/// aggregate result, passing a result pointer to a fn that returns a
/// register, so the field was never written (gf16_dot4.t27, d6_test.t27).
#[test]
fn struct_field_initialised_from_a_scalar_call() {
    let src = "module sf;

const In = struct {
    a0: u16,
    b0: u16,
};

const Out = struct {
    result: u16,
};

fn mul(a: u16, b: u16) u16 {
    return a * b;
}

fn dot(inputs: In) Out {
    var r: Out = Out{ .result = mul(inputs.a0, inputs.b0) };
    return r;
}

fn direct(x: u16) Out {
    return Out{ .result = mul(x, x) };
}

test field_from_call {
    const o = dot(In{ .a0 = 3, .b0 = 5 });
    assert(o.result == 15);
    assert(direct(4).result == 16);
}

test overflow_in_field_call {
    const o = dot(In{ .a0 = 300, .b0 = 300 });
    assert(o.result == 0);
}
";
    let r = run(src);
    assert_eq!(names_ok(&r), vec![("field_from_call", false, true), ("overflow_in_field_call", false, false)]);
    assert_eq!(r[1].2.unwrap_err().0, TrapKind::Overflow);
}

#[test]
fn struct_rejections_are_precise() {
    let head = "module st;\n\nconst Pt = struct { x: u32, y: u32 };\n\n";
    let cases: &[(&str, &str, &str)] = &[
        ("test t { const p = Pt{ .x = 1 }; _ = p; }", "ExprStructLit", "missing field `y` of `Pt`"),
        ("test t { const p = Pt{ .x = 1, .y = 2, .z = 3 }; _ = p; }", "ExprStructLit", "`Pt` has no field `z`"),
        ("test t { const p = Pt{ .x = 1, .x = 2, .y = 3 }; _ = p; }", "ExprStructLit", "field `x` initialised twice"),
        ("test t { const p = .{ .x = 1, .y = 2 }; _ = p; }", "ExprStructLit", "anonymous `.{}` literal"),
        ("test t { const p = Pt{ .x = 1, .y = 2 }; p.x = 3; }", "StmtAssign", "assignment through a constant"),
        ("test t { const p = Pt{ .x = 1, .y = 2 }; assert(p == p); }", "type mismatch", "on a struct"),
        ("test t { const p = Pt{ .x = 1, .y = 2 }; assert(p.w == 1); }", "ExprFieldAccess", "`Pt` has no field `w`"),
        ("test t { assert(Color.red == 1); }", "ExprFieldAccess", "`Color.red`"),
        ("const L = struct { a: u32, l: L };\ntest t { const v: L = undefined; _ = v; }", "StructDecl", "`L` contains itself"),
        ("fn f(p: Pt) u32 { return p.x; }\ntest t { assert(f(3) == 3); }", "type mismatch", "expected Pt, found a scalar"),
    ];
    for (body, construct, detail) in cases {
        let m = rejected(&format!("{}{}\n", head, body));
        assert!(m.starts_with(&format!("t27b: unsupported construct {} at line", construct)), "{}: {}", body, m);
        assert!(m.contains(detail), "{}: {}", body, m);
    }
}

// --------------------------------------------------------------- strings

#[test]
fn strings_literals_params_fields_and_equality() {
    let src = "module s;

pub const NAME: str = \"clk\";
const OTHER: &str = \"clk\";
const EMPTY = \"\";
const N = NAME.len;

const Pin = struct { name: str, num: u32 };

fn is_denied(answer: str) bool {
    return answer == \"denied\";
}

fn pick(b: bool) str {
    if (b) {
        return \"yes\";
    }
    return \"no\";
}

fn pin_name(p: Pin) []const u8 {
    return p.name;
}

fn length(s: string) u64 {
    return s.len;
}

test consts_fold {
    assert(NAME == \"clk\");
    assert(NAME == OTHER);
    assert(NAME != \"clK\");
    assert(EMPTY.len == 0 and N == 3);
    assert(\"a b \".len == 4);
}

test runtime_compare {
    assert(is_denied(\"denied\"));
    assert(!is_denied(\"denie\"));
    assert(!is_denied(\"Denied\"));
    assert(!is_denied(\"\"));
    assert(pick(true) == \"yes\" and pick(false) != \"yes\");
    assert(length(pick(false)) == 2);
}

test locals_and_fields {
    var s: str = \"abc\";
    const t = s;
    s = \"tab\\tq\";
    assert(s.len == 5 and t.len == 3);
    assert(t == \"abc\" and s == \"tab\\tq\");
    var p = Pin{ .name = \"T9\", .num = 9 };
    assert(p.name == \"T9\");
    p.name = NAME;
    assert(pin_name(p) == NAME and pin_name(p).len == 3);
    var u = \"lit\";
    u = \"other\";
    assert(u.len == 5);
}

test unequal_fails {
    assert(is_denied(\"granted\"));
}
";
    let r = run(src);
    assert_eq!(
        names_ok(&r),
        vec![
            ("consts_fold", false, true),
            ("runtime_compare", false, true),
            ("locals_and_fields", false, true),
            ("unequal_fails", false, false),
        ]
    );
    assert_eq!(r[3].2, Err((TrapKind::Assert, 62)));
    let prog = lower_src(src).unwrap();
    let internal: Vec<&str> = prog.internal_abi.iter().map(|&i| prog.funcs[i as usize].name.as_str()).collect();
    assert_eq!(internal, vec!["is_denied", "pick", "pin_name", "length", lower::STR_EQL]);
    // The helper sits right after the source fns, before the tests.
    assert_eq!(prog.funcs[4].name, lower::STR_EQL);
    assert!(prog.funcs[5].is_test);
}

#[test]
fn string_rejections_are_precise() {
    let head = "module s;\n\nconst S: str = \"ab\";\n\n";
    let cases: &[(&str, &str, &str)] = &[
        ("test t { assert(S == 3); }", "type mismatch", "expected str, found a scalar"),
        ("test t { assert(S < \"b\"); }", "ExprBinary(<)", "on a string"),
        ("test t { assert(S.ptr == 0); }", "ExprFieldAccess(str)", "`.ptr` of a str"),
        ("test t { var s: str = \"x\"; s.len = 2; }", "StmtAssign", "assignment through a constant"),
        ("test t { assert(S); }", "condition", "expected bool, found a string"),
        ("const P = struct { s: str };\nconst Q = P{ .s = \"x\" };\ntest t { assert(Q.s.len == 1); }", "ConstDecl(str field)", "str field"),
        ("fn f() u32 { return 1; }\nconst T: str = f();\ntest t { assert(T.len == 0); }", "ConstDecl", "not a string literal"),
    ];
    for (body, construct, detail) in cases {
        let m = rejected(&format!("{}{}\n", head, body));
        assert!(m.starts_with(&format!("t27b: unsupported construct {} at line", construct)), "{}: {}", body, m);
        assert!(m.contains(detail), "{}: {}", body, m);
    }
}

// ---------------------------------------------------------------- arrays

#[test]
fn arrays_literals_indexing_len_and_for() {
    let src = "module a;

const N: u32 = 3;
const PRIMES: [4]u32 = [2, 3, 5, 7];
const FLAGS: [2]bool = [true, false];
pub const NAMES: [2]str = [\"in\", \"out\"];
const NONE: [0]str = [];

const Bus = struct { lanes: [N]u8, width: u16 };

fn total(a: [4]u32) u32 {
    var s: u32 = 0;
    for (a) |x| {
        s += x;
    }
    return s;
}

fn ramp(k: u8) [3]u8 {
    return [k, k + 1, k + 2];
}

fn at(a: [4]u32, i: u32) u32 {
    return a[i];
}

fn reset(a: [3]u8) [3]u8 {
    var r: [3]u8 = [0, 0, 0];
    r = a;
    r[0] = 1;
    return r;
}

fn fill(a: *[3]u8, v: u8) void {
    a[0] = v;
    a.*[2] = v;
}

test locals_and_indexing {
    var a: [4]i32 = [10, -20, 30, -40];
    a[1] = 5;
    a[3] += 1;
    var i: u32 = 2;
    assert(a[i] == 30);
    a[i] = a[0] + a[1];
    assert(a[2] == 15 and a[3] == -39);
    assert(a.len == 4 and PRIMES.len == 4);
    const b: [N]bool = [true, false, true];
    assert(b[0] and !b[1] and b[2]);
}

test consts_and_strings {
    assert(PRIMES[3] == 7 and at(PRIMES, 2) == 5);
    assert(FLAGS[0] and !FLAGS[1]);
    assert(NAMES[1] == \"out\" and NAMES.len == 2 and NONE.len == 0);
    var j: u32 = 0;
    assert(NAMES[j] == \"in\");
    var n: u64 = 0;
    for (NAMES) |s| {
        n += s.len;
    }
    assert(n == 5);
}

test copies_params_and_results {
    var a: [3]u8 = [1, 2, 3];
    const c = a;
    a[0] = 9;
    assert(c[0] == 1 and a[0] == 9);
    var r = ramp(4);
    assert(r[2] == 6 and ramp(1)[0] == 1);
    fill(&r, 0);
    assert(r[0] == 0 and r[1] == 5 and r[2] == 0);
    const d = reset(c);
    assert(d[0] == 1 and d[1] == 2 and c[0] == 1);
    assert(total(PRIMES) == 17 and total([1, 1, 1, 1]) == 4);
    var bus = Bus{ .lanes = [7, 8, 9], .width = 3 };
    bus.lanes[1] = 0;
    assert(bus.lanes[1] == 0 and bus.lanes.len == 3);
}

test for_with_break_and_continue {
    const a: [5]u32 = [1, 2, 3, 4, 5];
    var s: u32 = 0;
    for (a) |x| {
        if (x == 2) {
            continue;
        }
        if (x == 5) {
            break;
        }
        s += x;
    }
    assert(s == 8);
    var count: u32 = 0;
    for (a) |_| {
        count += 1;
    }
    assert(count == 5);
}

test index_out_of_bounds {
    const a: [3]u32 = [1, 2, 3];
    var i: u32 = 3;
    assert(a[i] == 0);
}
";
    let r = run(src);
    assert_eq!(
        names_ok(&r),
        vec![
            ("locals_and_indexing", false, true),
            ("consts_and_strings", false, true),
            ("copies_params_and_results", false, true),
            ("for_with_break_and_continue", false, true),
            ("index_out_of_bounds", false, false),
        ]
    );
    assert_eq!(r[4].2, Err((TrapKind::Bounds, 105)));
}

#[test]
fn array_rejections_are_precise() {
    let head = "module a;\n\nconst A: [3]u32 = [1, 2, 3];\n\n";
    let cases: &[(&str, &str, &str)] = &[
        ("test t { const b: [3]u32 = [1, 2]; _ = b; }", "ExprArrayLiteral", "2 elements for `[3]u32`"),
        ("test t { const b = [1, 2]; _ = b; }", "ExprArrayLiteral", "array literal with no result type"),
        ("test t { assert(A[3] == 0); }", "ExprIndex", "index 3 out of bounds for `[3]u32`"),
        ("test t { var i: i32 = 0; assert(A[i] == 1); }", "type mismatch", "expected u64"),
        ("test t { assert(A == A); }", "type mismatch", "on an array"),
        ("test t { var b: [3]u32 = A; b.len = 2; }", "ExprFieldAccess(.len)", "not a place"),
        ("test t { assert(A.ptr == 0); }", "ExprFieldAccess", "`.ptr` of an array"),
        ("test t { const s: str = \"ab\"; assert(s.ptr == 0); }", "ExprFieldAccess(str)", "`.ptr` of a str"),
        ("test t { const b: [N]u32 = undefined; _ = b; }", "type [N]T", "not a compile-time integer"),
        ("test t { A[0] = 2; }", "StmtAssign", "assignment through a constant"),
        ("test t { assert(A); }", "condition", "expected bool, found an array"),
    ];
    for (body, construct, detail) in cases {
        let m = rejected(&format!("{}{}\n", head, body));
        assert!(m.starts_with(&format!("t27b: unsupported construct {} at line", construct)), "{}: {}", body, m);
        assert!(m.contains(detail), "{}: {}", body, m);
    }
}

// ---------------------------------------------------------------- slices

/// The 1-based line of the first line of `src` containing `needle`.
fn line_of(src: &str, needle: &str) -> u32 {
    src.lines().position(|l| l.contains(needle)).expect("needle") as u32 + 1
}

#[test]
fn slices_params_slicing_len_index_and_for() {
    let src = "module a;

const Pair = struct { xs: []const u32, tag: u8 };

fn sum(xs: []const u32) u32 {
    var t: u32 = 0;
    for (xs) |x| {
        t += x;
    }
    return t;
}

fn fill(xs: []u32, v: u32) void {
    var i: u64 = 0;
    while (i < xs.len) {
        xs[i] = v;
        i += 1;
    }
}

fn tail(xs: []const u32) []const u32 {
    return xs[1..];
}

fn first(s: []const u8) u8 {
    return s[0];
}

test slice_of_array {
    var a: [5]u32 = [1, 2, 3, 4, 5];
    assert(sum(&a) == 15);
    assert(sum(a[1..3]) == 5);
    assert(sum(a[2..]) == 12);
    assert(sum(a[5..]) == 0 and sum(a[2..2]) == 0);
    const t = a[1..4];
    assert(t.len == 3 and t[0] == 2 and t[2] == 4);
    const u = t[1..];
    assert(u.len == 2 and u[1] == 4);
    assert(sum(tail(&a)) == 14);
    var lo: u64 = 1;
    var hi: u64 = 4;
    assert(sum(a[lo..hi]) == 9);
}

test writes_through_a_slice {
    var a: [4]u32 = [1, 2, 3, 4];
    fill(a[1..3], 7);
    assert(a[0] == 1 and a[1] == 7 and a[2] == 7 and a[3] == 4);
    const s: []u32 = a[0..2];
    s[1] = 9;
    assert(a[1] == 9);
    fill(&a, 0);
    assert(sum(&a) == 0);
}

test strings_index_and_slice {
    const s: []const u8 = \"hello\";
    assert(s.len == 5 and s[1] == 101);
    var i: u64 = 4;
    assert(s[i] == 111);
    assert(\"abc\"[2] == 99);
    assert(first(s[3..]) == 108);
    assert(s[1..3] == \"el\");
    var n: u32 = 0;
    for (s[0..2]) |c| {
        n += c;
    }
    assert(n == 104 + 101);
    var buf: [3]u8 = [120, 121, 122];
    assert(first(&buf) == 120 and buf[0..2] == \"xy\");
}

test slice_literals_and_fields {
    assert(sum(&[_]u32{ 10, 20 }) == 30);
    assert(sum(&[_]u32{}) == 0);
    const p = Pair{ .xs = &[_]u32{ 3, 4 }, .tag = 1 };
    assert(p.xs.len == 2 and p.xs[1] == 4 and sum(p.xs) == 7);
}

test index_past_the_end {
    var a: [3]u32 = [1, 2, 3];
    const s = a[0..2];
    var i: u64 = 2;
    assert(s[i] == 3);
}

test slice_end_past_the_length {
    var a: [3]u32 = [1, 2, 3];
    var j: u64 = 4;
    assert(sum(a[0..j]) == 0);
}

test slice_start_past_the_end {
    const s: []const u8 = \"abc\";
    var i: u64 = 2;
    assert(s[i..1].len == 0);
}
";
    let r = run(src);
    assert_eq!(
        names_ok(&r),
        vec![
            ("slice_of_array", false, true),
            ("writes_through_a_slice", false, true),
            ("strings_index_and_slice", false, true),
            ("slice_literals_and_fields", false, true),
            ("index_past_the_end", false, false),
            ("slice_end_past_the_length", false, false),
            ("slice_start_past_the_end", false, false),
        ]
    );
    assert_eq!(r[4].2, Err((TrapKind::Bounds, line_of(src, "assert(s[i] == 3)"))));
    assert_eq!(r[5].2, Err((TrapKind::Bounds, line_of(src, "a[0..j]"))));
    assert_eq!(r[6].2, Err((TrapKind::Bounds, line_of(src, "s[i..1]"))));
}

#[test]
fn slice_rejections_are_precise() {
    let head = "module a;\n\nconst A: [3]u32 = [1, 2, 3];\n\n";
    let cases: &[(&str, &str, &str)] = &[
        ("fn f(xs: []u32) u64 { return xs.len; }\ntest t { assert(f(&A) == 3); }", "type mismatch", "expected []u32, found a pointer"),
        ("fn f(xs: []const u32) u64 { return xs.len; }\ntest t { var b: [3]u32 = A; assert(f(b) == 3); }", "type mismatch", "expected []const u32, found [3]u32"),
        ("fn f(xs: []u8) u64 { return xs.len; }\ntest t { assert(f(\"ab\") == 2); }", "type mismatch", "expected []u8, found a string"),
        ("test t { assert(A[1..4].len == 3); }", "ExprIndex(slice)", "end 4 out of bounds for `[3]u32`"),
        ("test t { assert(A[2..1].len == 0); }", "ExprIndex(slice)", "start 2 is past end 1"),
        ("test t { assert(\"ab\"[2] == 0); }", "ExprIndex", "index 2 out of bounds for a string of length 2"),
        ("fn f(xs: []u32) void { xs.len = 0; }\ntest t { }", "StmtAssign", "assignment through a constant"),
        ("fn f(xs: []const u32) void { xs[0] = 1; }\ntest t { }", "StmtAssign", "assignment through a constant"),
        ("const S: []const u32 = A[0..];\ntest t { assert(S.len == 3); }", "ConstDecl(slice)", "module-level slice"),
        ("fn f(xs: []u32) u32 { return xs.ptr; }\ntest t { }", "ExprFieldAccess(slice)", "`.ptr` of a []u32"),
    ];
    for (body, construct, detail) in cases {
        let m = rejected(&format!("{}{}\n", head, body));
        assert!(m.starts_with(&format!("t27b: unsupported construct {} at line", construct)), "{}: {}", body, m);
        assert!(m.contains(detail), "{}: {}", body, m);
    }
}

// ------------------------------------------------------------------ enums

#[test]
fn plain_enums_as_integer_tags() {
    let src = r#"module enums;

const Op = enum(u8) {
    add = 1,
    sub,
    mul = 7,
};

const Trit = enum(i8) {
    neg = -1,
    zero = 0,
    pos = 1,
};

enum Dir {
    north,
    east,
    south,
    west,
}

const One = enum { only };

const Cell = struct {
    op: Op,
    t: Trit,
    n: u32,
};

const START: Dir = Dir.east;

fn next(d: Dir) Dir {
    if (d == .north) {
        return .east;
    }
    if (d == Dir.east) {
        return Dir::south;
    }
    if (d != .south) {
        return .north;
    }
    return .west;
}

fn neg(t: Trit) Trit {
    return @enumFromInt(-@intFromEnum(t));
}

fn code(o: Op) u8 {
    return @intFromEnum(o);
}

fn dir_of(x: u32) Dir {
    return @enumFromInt(x);
}

fn trit_of(x: i32) Trit {
    return @enumFromInt(x);
}

fn wide(d: Dir) u32 {
    return @intFromEnum(d);
}

fn step(d: Dir) Dir {
    var cur: Dir = d;
    cur = next(cur);
    cur = next(cur);
    return cur;
}

fn cell(o: Op) Cell {
    return Cell{ .op = o, .t = .pos, .n = 3 };
}

test tags {
    assert(@intFromEnum(Op.add) == 1);
    assert(@intFromEnum(Op.sub) == 2);
    assert(code(Op.mul) == 7);
    assert(@intFromEnum(Trit.neg) == -1);
    assert(@intFromEnum(Dir.west) == 3);
    assert(wide(Dir.south) == 2);
    assert(@intFromEnum(One.only) == 0);
}

test compare {
    assert(next(Dir.north) == Dir.east);
    assert(next(.east) == .south);
    assert(next(Dir.south) == Dir.west);
    assert(step(START) == Dir.west);
    assert(Dir.north < Dir.west);
    assert(Op.mul > Op.sub);
    assert(next(.west) != Dir.west);
    assert_eq(next(Dir.north), Dir.east);
    assert_eq(neg(.pos), .neg);
}

test conversions {
    assert(neg(Trit.neg) == Trit.pos);
    assert(neg(.zero) == .zero);
    assert(dir_of(3) == Dir.west);
    assert(trit_of(-1) == Trit.neg);
    assert(@as(Trit, .pos) == Trit.pos);
    assert(@as(u8, @intFromEnum(Dir.east)) == 1);
}

test fields {
    const c = cell(Op.sub);
    assert(c.op == Op.sub);
    assert(c.t == .pos);
    assert(@intFromEnum(c.op) + c.n == 5);
}

test bad_dir {
    assert(dir_of(4) == Dir.west);
}

test bad_trit {
    assert(trit_of(2) == Trit.pos);
}
"#;
    let r = run(src);
    assert_eq!(
        names_ok(&r),
        vec![
            ("tags", false, true),
            ("compare", false, true),
            ("conversions", false, true),
            ("fields", false, true),
            ("bad_dir", false, false),
            ("bad_trit", false, false),
        ]
    );
    // `@enumFromInt` of a value that is no tag traps, as Zig's safety check
    // does ("invalid enum value").
    assert_eq!(r[4].2, Err((TrapKind::EnumTag, line_of(src, "fn dir_of") + 1)));
    assert_eq!(r[5].2, Err((TrapKind::EnumTag, line_of(src, "fn trit_of") + 1)));
}

#[test]
fn enum_rejections_are_precise() {
    let head = "module a;\n\nenum Dir { north, east, south, west, }\n\n";
    let cases: &[(&str, &str, &str)] = &[
        ("const E = enum { a, b, pub const x = 1; };\ntest t { assert(E.a == E.a); }", "EnumDecl(method)", "`E` declares something inside it"),
        ("const E = enum(u8) { a, b, _ };\ntest t { assert(E.a == E.a); }", "EnumDecl(non-exhaustive)", "`E` has a `_` variant"),
        ("const U = union(enum) { a: u8, b: u32 };\ntest t { assert(1 == 1); }", "EnumDecl(union)", "tagged union `U`"),
        ("const E = enum(f32) { a, b };\ntest t { assert(E.a == E.a); }", "EnumDecl(tag type)", "`E` has tag type `f32`"),
        ("const E = enum(u8) { a = 300, b };\ntest t { assert(E.a == E.a); }", "EnumDecl", "`E.a` = 300 does not fit the tag type u8"),
        ("const E = enum(u8) { a = 1, b = 1 };\ntest t { assert(E.a == E.b); }", "EnumDecl", "`E.a` and `E.b` have the same tag 1"),
        ("fn f(d: Dir) u8 {\n    return switch (d) {\n        .north => 1,\n        else => 2,\n    };\n}\ntest t { assert(f(.north) == 1); }", "ExprSwitch", ""),
        ("fn f(a: Dir, b: Dir) bool { return a < b; }\ntest t { assert(f(.north, .east)); }", "ExprBinary(<) on enum", "two `Dir` values, which Zig does not order"),
        ("fn f(d: Dir) bool { return d < .west; }\ntest t { assert(f(.north)); }", "ExprBinary(<) on enum", "which Zig does not order"),
        ("test t { assert(Dir.north + 1 == 1); }", "ExprBinary(+) on enum", "arithmetic on an enum"),
        ("test t { assert(Dir.north == 0); }", "type mismatch", "`==` on Dir and a scalar"),
        ("test t { const x = .north; assert(x == Dir.north); }", "ExprEnumValue", "enum literal `.north` with no result type"),
        ("test t { assert(Dir.up == Dir.north); }", "ExprFieldAccess(enum)", "`Dir` has no variant `up`"),
        ("const D: Dir = @enumFromInt(9);\ntest t { assert(D == Dir.north); }", "ExprCall(@enumFromInt)", "9 is no tag of `Dir`"),
        ("test t { assert(@enumFromInt(1) == Dir.east); }", "ExprCall(@enumFromInt)", "with no enum result type"),
        ("const E = enum(u8) { a = 1, b = 5 };\nfn f(x: u8) E { return @enumFromInt(x); }\ntest t { assert(f(1) == E.a); }", "ExprCall(@enumFromInt)", "whose tags are not contiguous"),
        ("fn f(d: Dir) u8 { const x = @intFromEnum(d); return x; }\ntest t { assert(f(.east) == 1); }", "ExprCall(@intFromEnum auto-tag)", "the tag type of `Dir` is u2"),
        ("test t { assert(@intFromEnum(.north) == 0); }", "ExprCall(@intFromEnum)", "of `.north`, which has no enum type here"),
        ("fn f(d: Dir) u32 { return d; }\ntest t { assert(f(.east) == 1); }", "type mismatch", "found Dir"),
    ];
    for (body, construct, detail) in cases {
        let m = rejected(&format!("{}{}\n", head, body));
        assert!(m.starts_with(&format!("t27b: unsupported construct {} at line", construct)), "{}: {}", body, m);
        assert!(m.contains(detail), "{}: {}", body, m);
    }
}

// ------------------------------------------------------- module-level vars

#[test]
fn module_vars_are_fresh_per_test() {
    // t27c test-report runs every test in its own process, so each test sees
    // the initial values; the JIT and the interpreter reset them per entry.
    let src = "module mv;

const Mode = enum(u8) { idle, run, stop };
const Pt = struct { x: i32, y: i32 };

var counter: u32 = 14;
var flag: bool = false;
var small: u8 = 254;
var buf: [4]u32 = [1, 2, 3, 4];
var mode: Mode = .idle;
var pos: Pt = Pt { .x = 1, .y = -2 };

fn bump() {
    counter += 1;
    flag = !flag;
    mode = .run;
    pos.x = pos.x + 10;
}

fn poke(i: u32, v: u32) {
    buf[i] = v;
}

fn sum() u32 {
    var s: u32 = 0;
    var i: u32 = 0;
    while (i < 4) {
        s = s + buf[i];
        i = i + 1;
    }
    return s;
}

fn grow() u8 {
    small = small + 3;
    return small;
}

test first_bump {
    assert(counter == 14);
    bump();
    assert(counter == 15);
    assert(flag == true);
    assert(mode == .run);
    assert(pos.x == 11);
    assert(pos.y == -2);
}

test second_sees_fresh {
    assert(counter == 14);
    assert(flag == false);
    assert(mode == .idle);
    assert(pos.x == 1);
    var k: u32 = 0;
    while (k < 3) {
        counter = counter + 2;
        k = k + 1;
    }
    assert(counter == 20);
}

test array_state {
    assert(sum() == 10);
    poke(2, 100);
    assert(sum() == 107);
}

test array_fresh {
    assert(sum() == 10);
}

test overflow_traps {
    assert(grow() > 0);
}
";
    let r = run(src);
    assert_eq!(
        names_ok(&r),
        vec![
            ("first_bump", false, true),
            ("second_sees_fresh", false, true),
            ("array_state", false, true),
            ("array_fresh", false, true),
            ("overflow_traps", false, false),
        ]
    );
    assert!(matches!(r[4].2, Err((TrapKind::Overflow, _))), "{:?}", r[4].2);
}

#[test]
fn module_var_rejections_are_precise() {
    let head = "module a;\n\nvar g: u32 = 0;\n\n";
    let cases: &[(&str, &str, &str)] = &[
        // The reference reads the first top-level assignment in a test as a
        // fresh `const g = ..`, which shadows the var: a Zig compile error.
        ("test t { g = 5; assert(g == 5); }", "StmtAssign(module var in test)", "module-level var `g`"),
        ("test t { var g: u32 = 1; assert(g == 1); }", "StmtLocal(shadows module var)", "`g` shadows"),
        ("fn f(g: u32) u32 { g = g + 1; return g; }\ntest t { assert(f(1) == 2); }", "StmtLocal(shadows module var)", "`g` shadows"),
        ("fn f(g: u32) u32 { return g; }\ntest t { assert(g == 0); }", "ExprIdentifier(renamed module var)", "`g_arg`"),
        ("invariant i { assert(g == 0); }", "ExprIdentifier(var at comptime)", "module-level var `g`"),
        ("var h = 3;\ntest t { assert(h == 3); }", "VarDecl(module, untyped)", "`h` has no type"),
        ("const B: u32 = 2;\nvar h: u32 = B * 2;\ntest t { assert(h == 4); }", "VarDecl(module)", "not a compile-time integer"),
    ];
    for (body, construct, detail) in cases {
        let m = rejected(&format!("{}{}\n", head, body));
        assert!(m.starts_with(&format!("t27b: unsupported construct {} at line", construct)), "{}: {}", body, m);
        assert!(m.contains(detail), "{}: {}", body, m);
    }
    // A parameter the body only reads is renamed by the reference, so it
    // does not shadow; the following fn clears the rename.
    let ok = "module b;\n\nvar g: u32 = 7;\n\nfn f(g: u32) u32 { return g + 1; }\nfn h() u32 { return g; }\n\ntest t {\n    assert(f(1) == 2);\n    assert(h() == 7);\n}\n";
    assert_eq!(names_ok(&run(ok)), vec![("t", false, true)]);
}

#[test]
fn assert_with_message_checks_the_condition_only() {
    // t27c's Zig backend lowers `assert(cond, "msg")` to
    // `if (!(cond)) @panic("msg")`: the message never decides the verdict.
    let src = "module am;

fn twice(x: u32) u32 {
    return x * 2;
}

test holds {
    assert(twice(3) == 6, \"twice(3) is 6\");
}

test fails {
    assert(twice(3) == 7, \"twice(3) is not 7\");
}

invariant msg_invariant {
    assert(twice(1) == 2, \"invariant with a message\");
}
";
    let r = run(src);
    assert_eq!(
        names_ok(&r),
        vec![("holds", false, true), ("fails", false, false), ("msg_invariant", true, true)]
    );
    assert!(matches!(r[1].2, Err((TrapKind::Assert, 12))), "{:?}", r[1].2);
    // A message that is not a string literal is refused by name.
    let head = "module an;\n\nconst M: u32 = 1;\n\n";
    let cases: &[(&str, &str, &str)] = &[
        ("test t { assert(M == 1, M); }", "ExprCall(assert with non-literal message)", "not a string literal"),
    ];
    for (body, construct, detail) in cases {
        let m = rejected(&format!("{}{}\n", head, body));
        assert!(m.starts_with(&format!("t27b: unsupported construct {} at line", construct)), "{}: {}", body, m);
        assert!(m.contains(detail), "{}: {}", body, m);
    }
}

#[test]
fn undefined_stub_only_where_zig_never_looks() {
    // `fn f() { undefined; }` is the stub a port leaves where plumbing was.
    // t27c's Zig backend emits it as is; Zig rejects it only in a fn it
    // analyzes. Nothing a test reaches names `plumbing` or `stub` here, and
    // `pub` or `main` is not a root, so the reference runs both tests.
    let src = "module st;

fn stub() u32 {
    undefined;
}

fn plumbing() {
    undefined;
}

pub fn main() {
    plumbing();
    var x: u32 = stub();
}

fn inc(x: u32) u32 {
    return x + 1;
}

test inc_works {
    assert(inc(1) == 2);
}

test inc_fails {
    assert(inc(1) == 3);
}
";
    let r = run(src);
    assert_eq!(names_ok(&r), vec![("inc_works", false, true), ("inc_fails", false, false)]);
    // A stub something analyzed can reach (even through another fn, even on
    // a branch no test takes) makes the reference fail to compile: refused.
    let reached = [
        "fn stub() u32 { undefined; }\nfn f(x: u32) u32 { if (x > 9) { return stub(); } return x; }\ntest t { assert(f(1) == 1); }\n",
        "fn stub() { undefined; }\nfn g() { stub(); }\ninvariant i { g(); assert(true); }\n",
        "test t { undefined; }\n",
    ];
    for body in reached {
        let m = rejected(&format!("module sr;\n\n{}", body));
        assert!(
            m.starts_with("t27b: unsupported construct ExprIdentifier(undefined) statement at line"),
            "{}: {}",
            body,
            m
        );
    }
}

#[test]
fn ignored_value_only_where_zig_never_looks() {
    // A Rust-style tail expression (`fn f(v: u8) -> u32 { v }`) or a bare
    // comparison is emitted by t27c's Zig backend as `expr;`, with no
    // implicit return. Zig rejects that ("value of type 'bool' ignored") only
    // in a body it analyzes; nothing a test reaches names these fns, so the
    // reference compiles the file and runs both tests.
    let src = "module vi;

fn tail(v: u8) -> u32 {
    v
}

fn mixed(a: u32, b: u32) -> u32 {
    let c: u32 = a | b;
    c | 1
}

fn flag(x: u32) -> bool {
    if x > 3 {
        true
    } else {
        false
    }
}

fn inc(x: u32) u32 {
    return x + 1;
}

test inc_works {
    assert(inc(1) == 2);
}

test inc_fails {
    assert(inc(1) == 3);
}
";
    let r = run(src);
    assert_eq!(names_ok(&r), vec![("inc_works", false, true), ("inc_fails", false, false)]);
    // Where something analyzed reaches it -- a test, a brace-form invariant
    // (a `comptime` block), a fn a test calls -- the reference does not
    // compile: refused under the expression's own kind, never read as a
    // return value.
    let reached = [
        ("const N: u32 = 3;\ninvariant i { N == 3 }\n", "ExprBinary"),
        ("fn f(v: u32) -> u32 { v }\ntest t { assert(f(1) == 1); }\n", "ExprIdentifier"),
        ("fn g(a: u32) -> u32 { a + 1 }\nfn f(x: u32) -> u32 { return g(x); }\ntest t { assert(f(1) == 2); }\n", "ExprBinary"),
        ("test t { 1; }\n", "ExprLiteral"),
    ];
    for (body, kind) in reached {
        let m = rejected(&format!("module vr;\n\n{}", body));
        assert!(
            m.starts_with(&format!("t27b: unsupported construct {}(value ignored) statement at line", kind)),
            "{}: {}",
            body,
            m
        );
    }
}

// ------------------------------------------------------ stack parameters

/// More than 8 parameters: the ninth and later of a class are passed on the
/// stack, one full word each (t27b's own convention, so such a fn is not
/// exported). Every position is weighted differently so a slot read from
/// the wrong place changes the result; narrow and negative values check
/// that the callee re-normalises what it loads; a struct result's hidden
/// pointer is itself the ninth word; a recursive fn passes its own
/// parameters on; and calls happen with temps live across them.
#[test]
fn stack_parameters_reach_the_callee_in_order() {
    let src = "module sp;

const Pair = struct { lo: i32, hi: i32 };

fn w9(a: u32, b: u32, c: u32, d: u32, e: u32, f: u32, g: u32, h: u32, i: u32) u32 {
    return a + 2 * b + 3 * c + 4 * d + 5 * e + 6 * f + 7 * g + 8 * h + 100 * i;
}

fn mix(a: i8, b: u8, c: i16, d: u16, e: i32, f: u32, g: i64, h: u64, i: i8, j: u8, k: i16, l: bool, m: i64) i64 {
    var s: i64 = a;
    s = s * 3 + b;
    s = s * 3 + c;
    s = s * 3 + d;
    s = s * 3 + e;
    s = s * 3 + f;
    s = s * 3 + g;
    if (h > 5) {
        s = s + 1;
    }
    s = s * 3 + i;
    s = s * 3 + j;
    s = s * 3 + k;
    if (l) {
        s = s * 3 + 1;
    }
    return s * 3 + m;
}

fn pair8(a: i32, b: i32, c: i32, d: i32, e: i32, f: i32, g: i32, h: i32) Pair {
    return Pair{ .lo = a + b + c + d, .hi = e + f + g + 10 * h };
}

fn down(n: u32, a: u32, b: u32, c: u32, d: u32, e: u32, f: u32, g: u32, h: u32, acc: u32) u32 {
    if (n == 0) {
        return acc + h;
    }
    return down(n - 1, b, c, d, e, f, g, h, a, acc + a * n);
}

fn outer(x: u32) u32 {
    return (x + 1) + w9(x, x + 1, x + 2, x + 3, x + 4, x + 5, x + 6, x + 7, x + 8) + (x + 2);
}

test ninth_word {
    assert(w9(1, 1, 1, 1, 1, 1, 1, 1, 1) == 136);
    assert(w9(0, 0, 0, 0, 0, 0, 0, 0, 7) == 700);
}

test narrow_and_negative_on_the_stack {
    assert(mix(-1, 255, -300, 65535, -70000, 4000000000, -5, 6, -128, 200, -32768, true, 9) == mix(-1, 255, -300, 65535, -70000, 4000000000, -5, 6, -128, 200, -32768, true, 9));
    assert(mix(0, 0, 0, 0, 0, 0, 0, 0, -128, 0, 0, false, 0) == -128 * 27);
    assert(mix(0, 0, 0, 0, 0, 0, 0, 0, 0, 0, -1, false, 0) == -3);
    assert(mix(0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, true, 5) == 8);
}

test struct_result_pointer_on_the_stack {
    const p = pair8(1, 2, 3, 4, 5, 6, 7, 8);
    assert(p.lo == 10);
    assert(p.hi == 98);
}

test recursion_passes_its_stack_words_on {
    assert(down(0, 1, 2, 3, 4, 5, 6, 7, 8, 0) == 8);
    assert(down(3, 1, 2, 3, 4, 5, 6, 7, 8, 0) == 1 * 3 + 2 * 2 + 3 * 1 + 3);
}

test temps_live_across_the_call {
    assert(outer(10) == 11 + (10 + 22 + 36 + 52 + 70 + 90 + 112 + 136 + 1800) + 12);
}

test wrong_on_purpose {
    assert(w9(1, 2, 3, 4, 5, 6, 7, 8, 9) == 0);
}
";
    let r = run(src);
    assert_eq!(
        names_ok(&r),
        vec![
            ("ninth_word", false, true),
            ("narrow_and_negative_on_the_stack", false, true),
            ("struct_result_pointer_on_the_stack", false, true),
            ("recursion_passes_its_stack_words_on", false, true),
            ("temps_live_across_the_call", false, true),
            ("wrong_on_purpose", false, false),
        ]
    );
    let prog = lower_src(src).unwrap();
    let internal: Vec<&str> = prog.internal_abi.iter().map(|&i| prog.funcs[i as usize].name.as_str()).collect();
    assert_eq!(internal, vec!["pair8", "w9", "mix", "down"]);
}

#[test]
fn fn_declaration_rejections_are_precise() {
    let head = "module fd;\n\n";
    let many: Vec<String> = (0..65).map(|i| format!("p{}: u8", i)).collect();
    let cases: Vec<(String, &str, &str)> = vec![
        ("fn f(x: struct {}) u32 { return 1; }\ntest t { assert(true); }".into(), "FnDecl(untyped param)", "parameter `x` of `f`"),
        ("fn f(x: u32) u32 { return x; }\nfn f(x: u32) u32 { return x; }\ntest t { assert(f(1) == 1); }".into(), "FnDecl(duplicate)", "duplicate function `f`"),
        (format!("fn f({}) u8 {{ return p0; }}\ntest t {{ assert(true); }}", many.join(", ")), "FnDecl(too many params)", "`f` has 65 parameters, at most 64"),
    ];
    for (body, construct, detail) in &cases {
        let m = rejected(&format!("{}{}\n", head, body));
        assert!(m.starts_with(&format!("t27b: unsupported construct {} at line", construct)), "{}: {}", body, m);
        assert!(m.contains(detail), "{}: {}", body, m);
    }
}

/// f64 parameters past d7 and integer ones past x7 in one signature: each
/// class fills its own registers, and the overflow of both shares one run of
/// stack words in parameter order. A leaf callee and one that calls.
#[test]
fn f64_and_integer_stack_parameters_interleave() {
    let src = "module spf;

fn wf(a: f64, b: u8, c: f64, d: f64, e: i16, f: f64, g: f64, h: f64, i: f64, j: f64, k: i32, l: f64, m: u8, n: f64, o: u8, p: u8, q: u8, r: u8, s: i8) f64 {
    var t: f64 = a;
    t = t * 2.0 + c;
    t = t * 2.0 + d;
    t = t * 2.0 + f;
    t = t * 2.0 + g;
    t = t * 2.0 + h;
    t = t * 2.0 + i;
    t = t * 2.0 + j;
    t = t * 2.0 + l;
    t = t * 2.0 + n;
    var u: i32 = b;
    u = u * 3 + e;
    u = u * 3 + k;
    u = u * 3 + m;
    u = u * 3 + o;
    u = u * 3 + p;
    u = u * 3 + q;
    u = u * 3 + r;
    u = u * 3 + s;
    return t * 100000.0 + @as(f64, @floatFromInt(u));
}

fn half(x: f64) f64 {
    return x / 2.0;
}

fn wg(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64, g: f64, h: f64, i: f64, j: f64) f64 {
    return half(a) + b + c + d + e + f + g + h + 3.0 * i + 5.0 * j;
}

test leaf_callee {
    assert(wf(1.0, 0, 0.0, 0.0, 0, 0.0, 0.0, 0.0, 0.0, 0.0, 0, 0.0, 0, 0.0, 0, 0, 0, 0, 0) == 51200000.0);
    assert(wf(0.0, 0, 0.0, 0.0, 0, 0.0, 0.0, 0.0, 0.0, 0.0, 0, 0.0, 0, 1.0, 0, 0, 0, 0, 0) == 100000.0);
    assert(wf(0.0, 0, 0.0, 0.0, 0, 0.0, 0.0, 0.0, 0.0, 0.0, 0, 0.5, 0, 0.0, 0, 0, 0, 0, -1) == 99999.0);
    assert(wf(0.0, 1, 0.0, 0.0, -1, 0.0, 0.0, 0.0, 0.0, 0.0, 0, 0.0, 0, 0.0, 0, 0, 0, 0, 0) == 4374.0);
    assert(wf(0.0, 0, 0.0, 0.0, 0, 0.0, 0.0, 0.0, 0.0, 0.0, 0, 0.0, 0, 0.0, 0, 0, 0, 255, 0) == 765.0);
}

test calling_callee {
    assert(wg(2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0) == 1.0);
    assert(wg(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0) == 3.0);
    assert(wg(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -1.0) == -5.0);
}
";
    let r = run(src);
    assert_eq!(names_ok(&r), vec![("leaf_callee", false, true), ("calling_callee", false, true)]);
}
