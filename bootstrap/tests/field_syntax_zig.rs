//! #6451: three spellings gen-zig emitted verbatim, each a Zig syntax error
//! that stopped the whole file (measured on the t27b lab reference path at
//! master 03362c1da: 17 + 11 + 7 + 8 specs with one of them as first error).
//!
//! 1. A postfix optional type, `id: OrgID?`, is now `?OrgID`.
//! 2. A field named for a Zig keyword is escaped where it is initialised
//!    (`.@"error" = ...`) and read (`r.@"error"`), as it already was where
//!    it is declared.
//! 3. An array repeat literal `[0i32; 2]` is now `[_]i32{ 0 } ** 2`.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

fn scratch(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "t27c-fieldsyn-{tag}-{}-{}",
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

const SPEC: &str = "module field_syntax;\n\
struct Res { ok: bool, note: str?, error: str }\n\
fn make(ok: bool) -> Res { return Res { ok: ok, note: null, error: \"none\" }; }\n\
fn err_of(r: Res) -> str { return r.error; }\n\
fn sum_two() -> i32 { var xs: [i32; 2] = [0i32; 2]; xs[1] = 5; return xs[0] + xs[1]; }\n\
test optional_and_keyword { const r = make(true); assert(r.ok); assert(r.note == null); assert(err_of(r).len == 4); }\n\
test repeat { assert(sum_two() == 5); }\n";

#[test]
fn a_postfix_optional_type_is_a_zig_optional() {
    let z = gen_zig(SPEC);
    assert!(z.contains("note: ?[]const u8"), "postfix optional:\n{z}");
    assert!(!z.contains("str?"), "suffix kept:\n{z}");
}

#[test]
fn a_keyword_field_is_escaped_where_it_is_named() {
    let z = gen_zig(SPEC);
    assert!(z.contains(".@\"error\" = "), "initialiser:\n{z}");
    assert!(z.contains("r.@\"error\""), "field access:\n{z}");
}

#[test]
fn an_array_repeat_literal_is_a_zig_repeat() {
    let z = gen_zig(SPEC);
    assert!(z.contains("{ 0 } ** 2"), "repeat:\n{z}");
    assert!(!z.contains("0i32"), "rust suffix kept:\n{z}");
}

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
