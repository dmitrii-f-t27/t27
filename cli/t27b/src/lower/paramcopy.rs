//! Array and struct parameters the body assigns (spec:
//! `specs/tri/t27b/conformance/value_params.t27`).
//!
//! t27c's Zig backend renames such a parameter `<name>_arg` and opens the
//! body with `var <name> = <name>_arg;`, so the callee writes its own copy.
//! t27b passes an array or struct argument as a pointer to the caller's
//! memory; for these parameters the body starts by copying it into a slot
//! of its own, and the name is bound to that writable copy.
//!
//! "Assigns" is the reference's own test (`collect_mutable_names` in
//! `bootstrap/src/compiler.rs`): a statement whose target is the name, an
//! element `name[i]` or a field `name.f`, at the top of the body or inside
//! if / while / for. A parameter written only deeper (`name.f[i] = ...`)
//! is not renamed there, stays a constant, and the reference cannot compile
//! the write; so it is left read-only here and the write is refused.

use super::*;

impl<'a> Lower<'a> {
    /// The place parameter `pname` is bound to: `src` (the caller's memory),
    /// or a writable copy of it when the reference makes it a `var`.
    pub(super) fn param_place(&mut self, body: &[Node], pname: &str, src: Place, out: &mut Vec<Stmt>) -> R<Place> {
        if pname == "self" || !matches!(src.ty, LTy::Arr(..) | LTy::Struct(_)) || !reference_assigns(body, pname) {
            return Ok(src);
        }
        let k = self.new_slot(&src.ty)?;
        let dst = Place { addr: slot_expr(k), off: 0, ty: src.ty.clone(), mutable: true, temp: None };
        self.copy(&dst, src, out)?;
        Ok(dst)
    }
}

/// t27c's `collect_mutable_names`, for one name.
fn reference_assigns(stmts: &[Node], name: &str) -> bool {
    stmts.iter().any(|s| assigns_one(s, name))
}

fn assigns_one(s: &Node, name: &str) -> bool {
    match s.kind {
        NodeKind::StmtAssign => {
            let Some(lhs) = s.children.first() else { return false };
            let base = match lhs.kind {
                NodeKind::ExprIdentifier => Some(lhs),
                NodeKind::ExprIndex | NodeKind::ExprFieldAccess => lhs.children.first(),
                _ => None,
            };
            base.is_some_and(|b| b.kind == NodeKind::ExprIdentifier && b.name == name)
        }
        NodeKind::StmtIf | NodeKind::StmtWhile | NodeKind::StmtFor | NodeKind::StmtForRange => s.children.iter().any(|c| {
            if c.kind == NodeKind::Module {
                reference_assigns(&c.children, name)
            } else {
                assigns_one(c, name)
            }
        }),
        _ => false,
    }
}
