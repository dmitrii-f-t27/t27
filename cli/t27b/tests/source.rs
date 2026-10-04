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
        Some(Jit::load(&code, prog.funcs.len(), &prog.data).expect("jit load"))
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
