//! End-to-end tests from t27 source text: the t27c front-end, lowering, then
//! every `test` and `invariant` block run in the reference interpreter and, on
//! arm64 macOS, in the JIT, which must agree with it.
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
