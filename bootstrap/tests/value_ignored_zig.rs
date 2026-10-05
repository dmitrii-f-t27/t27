//! #6315: three source forms put a value where gen-zig wrote a bare statement,
//! and Zig refuses the file ("value of type 'bool' ignored"), so none of its
//! tests ran. Measured on the t27b lab reference path at master 03362c1da:
//! 19 specs blocked with this as their first error.
//!
//! 1. A brace invariant's predicate, `invariant i { MAX == 9 }`, landed in
//!    `comptime { MAX == 9; }`. It is now an assertion, as the clause form
//!    `invariant i: MAX == 9;` already was.
//! 2. A Rust-style tail expression, `fn f(v: u8) -> u32 { v }`, became `v;`.
//!    It is now `return v;` (gen-verilog has lowered it so since t27#1948),
//!    through a final if/else into each branch.
//! 3. A bare call to a module fn that returns a value, `load(1);`, is now
//!    `_ = load(1);`.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

fn scratch(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "t27c-valign-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("dir");
    d
}

fn gen_zig(src: &str) -> String {
    let dir = scratch("gen");
    let p = dir.join("m.t27");
    std::fs::write(&p, src).expect("write spec");
    let out = Command::new(env!("CARGO_BIN_EXE_t27c"))
        .arg("gen")
        .arg(&p)
        .output()
        .expect("run t27c");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "t27c gen failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).to_string()
}

const SPEC: &str = "module value_ignored;\n\
const MAX: u32 = 9;\n\
var LOADED: u32 = 0;\n\
fn ready() -> bool { return true; }\n\
fn top_tail(v: u32) -> u32 { v + 1 }\n\
fn branch_tail(v: u32) -> u32 { if v == 0 { 7 } else { v } }\n\
fn load(v: u32) -> bool { LOADED = v; return true; }\n\
invariant max_is_nine { MAX == 9 }\n\
invariant ready_holds { ready() }\n\
test tails { assert(top_tail(1) == 2); assert(branch_tail(0) == 7); assert(branch_tail(3) == 3); }\n\
test discard { load(5); assert(LOADED == 5); }\n";

#[test]
fn a_brace_invariant_predicate_is_asserted() {
    let z = gen_zig(SPEC);
    assert!(!z.contains("    MAX == 9;"), "bare predicate kept:\n{z}");
    assert!(z.contains("if (!(MAX == 9))"), "predicate not asserted:\n{z}");
    assert!(z.contains("if (!(ready()))"), "bool call not asserted:\n{z}");
}

#[test]
fn a_tail_expression_is_the_return_value() {
    let z = gen_zig(SPEC);
    assert!(z.contains("return v + 1;") || z.contains("return (v + 1);"), "top tail:\n{z}");
    assert!(z.contains("return 7;"), "then-branch tail:\n{z}");
    assert!(z.contains("return v;"), "else-branch tail:\n{z}");
}

#[test]
fn a_bare_value_call_is_discarded_by_name() {
    let z = gen_zig(SPEC);
    assert!(z.contains("_ = load(5);"), "value call not discarded:\n{z}");
}

/// The statement about the language: Zig accepts the file and its tests pass.
#[test]
fn zig_runs_the_generated_tests() {
    let zig_ok = Command::new("zig")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !zig_ok {
        eprintln!("zig not on PATH -- SKIPPED, and saying so rather than passing silently");
        return;
    }
    let z = gen_zig(SPEC);
    let dir = scratch("zig");
    let f = dir.join("m.zig");
    std::fs::write(&f, &z).expect("write zig");
    let out = Command::new("zig")
        .arg("test")
        .arg(&f)
        .current_dir(&dir)
        .env("ZIG_GLOBAL_CACHE_DIR", dir.join("gcache"))
        .env("ZIG_LOCAL_CACHE_DIR", dir.join("lcache"))
        .output()
        .expect("run zig");
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "zig test failed:\n{err}\n--- source ---\n{z}");
}
