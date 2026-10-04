//! Lowering: t27c AST -> t27b IR.
//!
//! This is where the supported subset is enforced. Anything outside it is
//! reported as a `Reject` naming the AST construct and the nearest known source
//! line; nothing outside the subset reaches the back end.
//!
//! Typing follows Zig, which is what the t27 reference backend emits:
//! integer literals are comptime integers (exact, arbitrary sign) that take the
//! type of the typed operand they meet and must fit in it; two typed operands
//! must agree up to lossless widening (same signedness and not narrower, or
//! unsigned into a strictly wider signed type); `if`/`while` conditions are
//! `bool`; `bool` and integers never mix.
//!
//! Memory: a struct value lives in memory, never in a register -- a frame
//! slot for a local or a temporary, the caller's memory for a parameter (passed
//! as a pointer, never written), read-only data for a module constant. A
//! function returning a struct takes a hidden last pointer parameter to the
//! caller's result slot, builds the value there and returns that pointer.
//! Struct layout is C's: fields in order, each at its own alignment.
//!
//! Strings: `str` (also spelled `string`, `&str`, `[]const u8`) is Zig's
//! `[]const u8`, a 16-byte aggregate in memory -- the address of the bytes at
//! offset 0, the length (u64) at offset 8 -- passed, returned and copied like
//! a struct. A string literal is a compile-time value (`Val::S`) whose bytes
//! sit in read-only data; it is written into memory only where a `str` place
//! needs it. `==` and `!=` on strings compare contents, as t27c's Zig backend
//! does with `std.mem.eql`: two literals fold, anything else calls one
//! synthesized IR function, `__t27b_str_eql`.
//!
//! Arrays: `[N]T` (N a literal or an integer constant) is N elements of T back
//! to back in memory, an aggregate like a struct: copied on assignment, passed
//! by pointer, returned through the result slot. An array literal takes the
//! type it is assigned to (t27c's Zig backend writes `.{...}`), so one with no
//! result type is refused, and its element count must be N. An index is a
//! u64; a constant one out of range is refused (Zig refuses it at compile
//! time), a runtime one is checked and traps as `index out of bounds`. `.len`
//! is the constant N. `for (a) |x|` walks the elements with a hidden index. A
//! module constant array of strings cannot live in read-only data (it holds
//! addresses), so it stays a compile-time value (`Val::A`) and is written into
//! a frame temporary only where memory is needed.
//!
//! Slices: `[]T` and `[]const T` are Zig's slices, laid out like `str` (which
//! is exactly `[]const u8`): a 16-byte aggregate, the address of the first
//! element at offset 0 and the length (u64) at offset 8. Whether the elements
//! may be written is part of the type, not of the place holding the slice.
//! `a[i..j]` (and the open `a[i..]`) slices an array, a pointer to an array, a
//! slice or a string: the end is checked against the length and the start
//! against the end, both as `index out of bounds`. Indexing a slice or a
//! string is checked against its runtime length. `*[N]T` coerces to `[]T` and
//! `[]const T`, `[]T` to `[]const T`, and `&[_]T{ ... }` to `[]const T`.
//! `for (s) |x|` reads the slice's address and length once, before the loop.

use crate::codegen;
use crate::compiler::{Node, NodeKind};
use crate::ir::*;
use std::collections::{HashMap, HashSet};

/// A construct outside the supported subset (or a type error inside it).
#[derive(Clone, Debug)]
pub struct Reject {
    /// The AST node kind or a short category such as `type f32`.
    pub construct: String,
    pub line: u32,
    pub detail: String,
}

impl Reject {
    pub fn message(&self) -> String {
        let mut s = format!(
            "t27b: unsupported construct {} at line {}",
            self.construct, self.line
        );
        if !self.detail.is_empty() {
            s.push_str(&format!(" ({})", self.detail));
        }
        s
    }
}

type R<T> = Result<T, ()>;

/// A lowered expression: a comptime integer (exact, untyped), a typed
/// scalar IR expression, a pointer (`Ty::Ptr` expression and its `LTy::Ptr`
/// type), or a struct in memory.
#[derive(Clone, Debug)]
enum Val {
    Ct(i128),
    E(Expr),
    P(Expr, LTy),
    M(Place),
    /// A compile-time string: read-only blob `k` (its bytes, then one NUL
    /// byte that is not part of the string) and the length in bytes.
    S(u32, u64),
    /// A compile-time array whose elements hold strings (`LTy::Arr` type,
    /// one `S` or nested `A` per element): read-only data cannot hold the
    /// address of a string, so the array is written into a frame temporary
    /// wherever memory is needed, and a constant index folds to the element.
    A(LTy, Vec<Val>),
    /// Recovery mode only (`blockers`): the value of an expression that was
    /// already rejected. Anything built from it is dropped without a second
    /// report, so one unsupported construct is named once, not once per use.
    Poison,
}

impl Val {
    fn is_poison(&self) -> bool {
        matches!(self, Val::Poison)
    }
}

#[derive(Clone, Debug)]
enum Binding {
    Var { id: VarId, mutable: bool },
    Const(Val),
    /// A variable that lives in memory: a struct, or a scalar whose address
    /// is taken.
    Mem(Place),
}

/// A source type: a scalar or a pointer (one register), or a struct.
#[derive(Clone, Debug, PartialEq, Eq)]
enum LTy {
    S(Ty),
    /// `*T` (true: mutable) or `*const T` (false).
    Ptr(Box<LTy>, bool),
    Struct(u32),
    /// `str`: `[]const u8`, a (pointer, length) pair in memory.
    Str,
    /// `[N]T`: N elements of T, back to back, in memory.
    Arr(Box<LTy>, u32),
    /// `[]T` (true: elements writable) or `[]const T` (false): a (pointer,
    /// length) pair in memory. `[]const u8` is `Str`, never this.
    Slice(Box<LTy>, bool),
    /// A plain enum (index into `Lower::enums`): its tag, an integer of
    /// the register type given, in one register.
    Enum(u32, Ty),
}

/// `ty` at `addr + off`. `addr` is evaluated exactly once by whatever
/// consumes the place, so it may be a call or a `Seq`.
#[derive(Clone, Debug)]
struct Place {
    addr: Expr,
    off: u32,
    ty: LTy,
    mutable: bool,
    /// The frame slot this place is the whole of, when it is a temporary
    /// nothing else refers to (a struct literal, a call's result).
    temp: Option<u32>,
}

#[derive(Clone, Debug)]
struct Field<'a> {
    name: String,
    ty: LTy,
    off: u32,
    default: Option<&'a Node>,
}

struct StructDef<'a> {
    name: String,
    fields: Vec<Field<'a>>,
    /// None while the layout is being computed (or after it failed).
    size: Option<u32>,
    align: u32,
    /// Why the layout failed, to report again at every later use.
    fail: Option<Reject>,
}

/// A plain enum, lowered to its integer tag.
struct EnumDef {
    name: String,
    /// The register type holding the tag.
    tag: Ty,
    /// The width of Zig's tag type: `tag`'s own width, or for an enum
    /// declared with no tag type (Zig picks the smallest unsigned integer
    /// that holds every tag) possibly fewer bits, which t27b has no type for.
    bits: u32,
    /// (name, tag value), in declaration order.
    variants: Vec<(String, i128)>,
}

impl EnumDef {
    /// `@intFromEnum` gives a value of a type t27b has.
    fn exact(&self) -> bool {
        self.bits == self.tag.bits()
    }

    fn value(&self, name: &str) -> Option<i128> {
        self.variants.iter().find(|v| v.0 == name).map(|v| v.1)
    }
}

/// Where a runtime `@intFromEnum` of an enum whose tag type t27b has no type
/// for (`u2`, say) is used: only where its storage type gives the same answer.
#[derive(Clone, Copy, PartialEq)]
enum TagUse {
    /// Nowhere special: refused, since arithmetic on it would overflow at a
    /// different point.
    Value,
    /// An operand of `==`, `<`, ... or `assert_eq`: exact at any width.
    Compare,
    /// Coerced to this integer type, which Zig allows when it holds every
    /// value of the tag type.
    Want(Ty),
}

struct Sig {
    id: FuncId,
    params: Vec<LTy>,
    ret: Option<LTy>,
    /// Recovery mode only: the signature named a type outside the subset.
    /// Calls to it are dropped silently; its body is still lowered.
    poisoned: bool,
}

struct Lower<'a> {
    mode: OverflowMode,
    sites: Vec<Site>,
    errors: Vec<Reject>,
    sigs: HashMap<String, Sig>,
    globals: HashMap<String, Val>,
    const_nodes: HashMap<String, &'a Node>,
    resolving: HashSet<String>,
    struct_nodes: HashMap<String, &'a Node>,
    structs: Vec<StructDef<'a>>,
    struct_ids: HashMap<String, u32>,
    /// Enum declarations, built on first use (all of them after pass 1).
    enum_nodes: HashMap<String, &'a Node>,
    enums: Vec<EnumDef>,
    enum_ids: HashMap<String, u32>,
    /// Why an enum declaration was refused, to report again at a later use.
    enum_fail: HashMap<String, Reject>,
    data: Vec<Vec<u8>>,
    internal_abi: Vec<FuncId>,
    /// Blob of each string literal's bytes, so equal literals share one.
    strings: HashMap<Vec<u8>, u32>,
    /// The number of source fns: the id `__t27b_str_eql` gets if used.
    nfuncs: FuncId,
    /// `__t27b_str_eql` is called somewhere.
    eql_used: bool,
    // Per-function state.
    vars: Vec<Var>,
    /// The source type of each variable (parallel to `vars`).
    ltys: Vec<LTy>,
    slots: Vec<SlotInfo>,
    /// Names whose address is taken somewhere in the body (`&x`): such a
    /// scalar lives in a frame slot instead of a register.
    addr_taken: HashSet<String>,
    /// The hidden result pointer of a function returning a struct.
    sret: Option<VarId>,
    scopes: Vec<HashMap<String, Binding>>,
    loop_depth: u32,
    line: u32,
    ret: Option<LTy>,
    in_test: bool,
    test_assigns: HashMap<String, u32>,
    /// The source text, when known: a clause-form `invariant` (and a
    /// braceless `test`) reaches lowering with no line on any of its nodes, so
    /// its header line is looked up here instead.
    src: Option<&'a str>,
    // Recovery mode (`blockers`): keep lowering after a rejection so every
    // unsupported construct in the file is named, not only the first per item.
    recover: bool,
    /// Names declared by a rejected top-level item (a struct, an enum, a const
    /// outside the subset): a use of one is not reported again.
    poison_names: HashSet<String>,
    /// Names of top-level struct and enum declarations, so a type that names
    /// one is reported as `type (struct)` / `type (enum)`.
    type_decls: HashMap<String, &'static str>,
    /// The current fn's return type was rejected.
    ret_poison: bool,
}

/// Lower a parsed module. All rejected constructs are returned (at most one per
/// top-level item, since lowering of an item stops at its first rejection).
pub fn lower(ast: &Node, mode: OverflowMode) -> Result<Program, Vec<Reject>> {
    lower_src(ast, mode, None)
}

/// `lower`, with the source text the AST was parsed from, for line numbers.
pub fn lower_src<'a>(ast: &'a Node, mode: OverflowMode, src: Option<&'a str>) -> Result<Program, Vec<Reject>> {
    lower_mode(ast, mode, src, false)
}

/// Every construct outside the subset in a module, not only the first per
/// item: lowering continues past each rejection (statement by statement, and
/// operand by operand inside an expression), and a value built from a rejected
/// one is dropped without a second report. Empty when the module lowers.
///
/// A rejected top-level declaration (a `struct`, an `enum`) is still one
/// entry: its members are not looked into, so a file's list is a lower bound.
pub fn blockers(ast: &Node, mode: OverflowMode) -> Vec<Reject> {
    blockers_src(ast, mode, None)
}

/// `blockers`, with the source text the AST was parsed from, for line numbers.
pub fn blockers_src<'a>(ast: &'a Node, mode: OverflowMode, src: Option<&'a str>) -> Vec<Reject> {
    match lower_mode(ast, mode, src, true) {
        Ok(_) => Vec::new(),
        Err(r) => r,
    }
}

fn lower_mode<'a>(
    ast: &'a Node,
    mode: OverflowMode,
    src: Option<&'a str>,
    recover: bool,
) -> Result<Program, Vec<Reject>> {
    let mut l = Lower {
        mode,
        sites: vec![Site {
            kind: TrapKind::Overflow,
            line: 0,
            what: "none".into(),
            ty: Ty::Bool,
        }],
        errors: Vec::new(),
        sigs: HashMap::new(),
        globals: HashMap::new(),
        const_nodes: HashMap::new(),
        resolving: HashSet::new(),
        struct_nodes: HashMap::new(),
        structs: Vec::new(),
        struct_ids: HashMap::new(),
        enum_nodes: HashMap::new(),
        enums: Vec::new(),
        enum_ids: HashMap::new(),
        enum_fail: HashMap::new(),
        data: Vec::new(),
        internal_abi: Vec::new(),
        strings: HashMap::new(),
        nfuncs: 0,
        eql_used: false,
        vars: Vec::new(),
        ltys: Vec::new(),
        slots: Vec::new(),
        addr_taken: HashSet::new(),
        sret: None,
        scopes: Vec::new(),
        loop_depth: 0,
        line: ast.line,
        ret: None,
        in_test: false,
        test_assigns: HashMap::new(),
        src,
        recover,
        poison_names: HashSet::new(),
        type_decls: HashMap::new(),
        ret_poison: false,
    };
    let module = if ast.kind == NodeKind::Module {
        ast.name.clone()
    } else {
        String::new()
    };
    let items: Vec<&Node> = if ast.kind == NodeKind::Module {
        ast.children.iter().collect()
    } else {
        vec![ast]
    };

    // Struct declarations are laid out on first use, like Zig's lazy
    // analysis: an unused struct with an unsupported member rejects nothing.
    for item in &items {
        if item.kind == NodeKind::StructDecl {
            l.struct_nodes.insert(item.name.clone(), item);
        }
        if item.kind == NodeKind::EnumDecl && !item.name.is_empty() {
            l.enum_nodes.insert(item.name.clone(), item);
        }
    }

    // Pass 1: signatures and constant declarations.
    let mut fn_nodes: Vec<&Node> = Vec::new();
    let mut next_id: FuncId = 0;
    for item in &items {
        match item.kind {
            NodeKind::StructDecl => {
                l.type_decls.insert(item.name.clone(), "struct");
            }
            NodeKind::EnumDecl => {
                l.type_decls.insert(item.name.clone(), "enum");
            }
            NodeKind::ConstDecl if is_tagged_union(item) => {
                l.type_decls.insert(item.name.clone(), "tagged union");
            }
            // A constant used where a type goes is a type alias.
            NodeKind::ConstDecl => {
                l.type_decls.insert(item.name.clone(), "alias");
            }
            _ => {}
        }
    }
    for item in &items {
        l.see(item);
        match item.kind {
            NodeKind::FnDecl => {
                if l.sigs.contains_key(&item.name) {
                    let _: R<()> = l.reject(
                        "FnDecl",
                        format!("duplicate function `{}`", item.name),
                    );
                    continue;
                }
                match l.signature(item) {
                    Ok((params, ret)) => {
                        if params.iter().any(is_agg) || ret.as_ref().is_some_and(is_agg) {
                            l.internal_abi.push(next_id);
                        }
                        l.sigs.insert(
                            item.name.clone(),
                            Sig {
                                id: next_id,
                                params,
                                ret,
                                poisoned: false,
                            },
                        );
                        next_id += 1;
                        fn_nodes.push(item);
                    }
                    Err(()) if l.recover => {
                        // Keep the body: what it contains is reported too.
                        let n = item.params.len();
                        l.sigs.insert(
                            item.name.clone(),
                            Sig {
                                id: next_id,
                                params: vec![LTy::S(Ty::Bool); n],
                                ret: None,
                                poisoned: true,
                            },
                        );
                        next_id += 1;
                        fn_nodes.push(item);
                    }
                    Err(()) => {
                        l.poison_names.insert(item.name.clone());
                    }
                }
            }
            NodeKind::TestBlock | NodeKind::InvariantBlock | NodeKind::BenchBlock | NodeKind::StructDecl => {}
            // Built after pass 1, once every constant a tag may name is known.
            NodeKind::EnumDecl if !item.name.is_empty() => {}
            // `union(enum) { ... }`: t27c keeps only its text.
            NodeKind::ConstDecl if is_tagged_union(item) => {
                if let Some(l2) = l.src.and_then(|s| decl_line(s, &item.name)) {
                    l.line = l2;
                }
                let _: R<()> = l.reject(
                    "EnumDecl(union)",
                    format!("tagged union `{}`; only plain enums are supported", item.name),
                );
                l.poison_names.insert(item.name.clone());
            }
            NodeKind::ConstDecl => {
                l.const_nodes.insert(item.name.clone(), item);
            }
            // `use` declarations were already resolved by splicing the
            // imported declarations into the source (front::parse).
            NodeKind::UseDecl => {}
            // The t27c parser reads a dotted `module a.b;` or `use a.b;` as
            // `a` followed by a stray top-level expression `.b`, with no line.
            NodeKind::StmtExpr
                if matches!(item.children.first(), Some(c) if c.kind == NodeKind::ExprEnumValue) =>
            {
                let name = &item.children[0].name;
                let _: R<()> = l.reject(
                    "StmtExpr",
                    format!(
                        "stray `.{}` at top level; t27c parses a dotted `module a.b;` or `use a.b;` as `a` followed by `.b`",
                        name
                    ),
                );
            }
            _ => {
                let k = kind_name(item);
                let detail = if k.starts_with("Stmt") || k.starts_with("Expr") {
                    "statement at top level, outside any fn or test".to_string()
                } else {
                    String::new()
                };
                let _: R<()> = l.reject(&k, detail);
                if !item.name.is_empty() {
                    l.poison_names.insert(item.name.clone());
                }
            }
        }
    }
    l.nfuncs = next_id;
    let mut const_names: Vec<String> = l.const_nodes.keys().cloned().collect();
    const_names.sort();
    for name in const_names {
        let _ = l.global(&name);
    }
    let mut enum_names: Vec<String> = l.enum_nodes.keys().cloned().collect();
    enum_names.sort();
    for name in enum_names {
        if !l.enum_ids.contains_key(&name) && !l.enum_fail.contains_key(&name) {
            let _ = l.enum_id(&name);
        }
    }

    // Pass 2: bodies.
    let mut funcs: Vec<Func> = Vec::new();
    for item in &fn_nodes {
        if let Ok(f) = l.function(item) {
            funcs.push(f);
        }
    }
    let mut unchecked = Vec::new();
    let mut benches: Vec<Func> = Vec::new();
    for item in &items {
        match item.kind {
            NodeKind::TestBlock => {
                if let Ok(f) = l.test(item, false) {
                    funcs.push(f);
                }
            }
            // Lowered and compiled below, never run and never a test.
            NodeKind::BenchBlock => {
                if let Ok(f) = l.bench(item) {
                    benches.push(f);
                }
            }
            // An invariant with no lowered body is one whose clause the
            // front-end discarded (t27c's Zig backend writes `NOT CHECKED`
            // for exactly this case): there is nothing to run, and running
            // nothing must not be reported as the invariant holding.
            NodeKind::InvariantBlock if item.children.is_empty() && item.extra_field != "partial" => {
                unchecked.push(item.name.clone());
            }
            NodeKind::InvariantBlock => {
                if let Ok(f) = l.test(item, true) {
                    funcs.push(f);
                }
            }
            _ => {}
        }
    }
    if !l.errors.is_empty() {
        return Err(l.errors);
    }
    if l.eql_used {
        // Source fns are ids 0..nfuncs and come first in `funcs`, in order.
        let site = l.site(TrapKind::NoReturn, format!("end of fn {}", STR_EQL), Ty::Bool);
        funcs.insert(l.nfuncs as usize, str_eql_func(site));
        l.internal_abi.push(l.nfuncs);
    }
    let prog = Program {
        module,
        funcs,
        sites: l.sites,
        mode,
        unchecked,
        data: l.data,
        internal_abi: l.internal_abi,
    };
    // Bench bodies are compiled to machine code, so a body that lowers but
    // that the code generator refuses still rejects the file, and then
    // dropped: the program that runs is exactly the one without them.
    if !benches.is_empty() {
        let mut with = prog.clone();
        let first = with.funcs.len();
        with.funcs.extend(benches);
        let mut errors = Vec::new();
        for id in first..with.funcs.len() {
            if let Err(e) = codegen::compile_func(&with, id as FuncId, codegen::TrapStyle::Jit) {
                errors.push(Reject {
                    construct: e.construct.to_string(),
                    line: e.line,
                    detail: format!("bench {}: {}", with.funcs[id].name, e.detail),
                });
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
    }
    Ok(prog)
}

/// Line (1-based) of the first `<keyword> <name>` header in `src`, the name
/// optionally quoted.
fn header_line(src: &str, keyword: &str, name: &str) -> Option<u32> {
    for (i, line) in src.lines().enumerate() {
        let Some(rest) = line.trim_start().strip_prefix(keyword) else { continue };
        let Some(rest) = rest.strip_prefix(|c: char| c == ' ' || c == '\t') else { continue };
        let rest = rest.trim_start();
        let rest = rest.strip_prefix('"').unwrap_or(rest);
        if let Some(after) = rest.strip_prefix(name) {
            if !after.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_') {
                return Some(i as u32 + 1);
            }
        }
    }
    None
}

/// Line (1-based) of the declaration of `name`: `const`, `pub const`,
/// `enum` or `pub enum`.
fn decl_line(src: &str, name: &str) -> Option<u32> {
    ["const", "pub const", "enum", "pub enum"]
        .iter()
        .filter_map(|k| header_line(src, k, name))
        .min()
}

/// t27c reads `const U = union(enum) { ... };` as a `ConstDecl` with no value
/// and the declaration's tokens as text.
fn is_tagged_union(n: &Node) -> bool {
    n.kind == NodeKind::ConstDecl && n.children.is_empty() && n.value.replace(' ', "").starts_with("union(enum")
}

fn kind_name(n: &Node) -> String {
    format!("{:?}", n.kind)
}

/// Parse an integer literal: decimal, 0x, 0o, 0b, with `_` separators.
fn parse_int(s: &str) -> Option<i128> {
    let t: String = s.chars().filter(|c| *c != '_').collect();
    let (digits, radix) = if let Some(r) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        (r.to_string(), 16)
    } else if let Some(r) = t.strip_prefix("0o") {
        (r.to_string(), 8)
    } else if let Some(r) = t.strip_prefix("0b") {
        (r.to_string(), 2)
    } else {
        (t.clone(), 10)
    };
    if digits.is_empty() {
        return None;
    }
    let v = u128::from_str_radix(&digits, radix).ok()?;
    if v > i128::MAX as u128 {
        return None;
    }
    Some(v as i128)
}

impl<'a> Lower<'a> {
    fn see(&mut self, n: &Node) {
        if n.line != 0 {
            self.line = n.line;
        }
    }

    fn reject<T>(&mut self, construct: &str, detail: String) -> R<T> {
        self.errors.push(Reject {
            construct: construct.to_string(),
            line: self.line,
            detail,
        });
        Err(())
    }

    fn site(&mut self, kind: TrapKind, what: String, ty: Ty) -> SiteId {
        self.sites.push(Site {
            kind,
            line: self.line,
            what,
            ty,
        });
        (self.sites.len() - 1) as SiteId
    }

    fn ty(&mut self, name: &str) -> R<Ty> {
        let t = name.trim();
        match Ty::from_name(t) {
            Some(ty) => Ok(ty),
            None => {
                let (construct, detail) = self.type_construct(t);
                self.reject(&construct, detail)
            }
        }
    }

    /// The construct a type outside the subset is reported as. Types of one
    /// shape share a name (`type [N]T`, `type []T`, `type (struct)`), so the
    /// count says how many files need that shape; the detail names the type.
    fn type_construct(&self, t: &str) -> (String, String) {
        let shape = if let Some(k) = self.type_decls.get(t) {
            format!("type ({})", k)
        } else if t.starts_with("[]") {
            "type []T".to_string()
        } else if t.starts_with('[') {
            "type [N]T".to_string()
        } else if t.starts_with('(') {
            "type (tuple)".to_string()
        } else if t.starts_with('?') {
            "type ?T".to_string()
        } else if t.starts_with('*') {
            "type *T".to_string()
        } else if t.contains('!') {
            "type E!T".to_string()
        } else if t.starts_with("struct") {
            "type (anonymous struct)".to_string()
        } else if t.contains('(') {
            "type (generic)".to_string()
        } else if t.starts_with(|c: char| c.is_ascii_uppercase()) && !matches!(t, "Result" | "Option") {
            // Declared nowhere this file can see: an import `use` did not
            // splice, or a type of a sibling spec.
            "type (undeclared)".to_string()
        } else {
            return (format!("type {}", t), String::new());
        };
        (shape, format!("`{}`", t))
    }

    fn signature(&mut self, n: &Node) -> R<(Vec<LTy>, Option<LTy>)> {
        let mut bad = false;
        let mut params = Vec::new();
        for (pname, pty) in &n.params {
            let r = if pname.starts_with("comptime ") || pty.is_empty() {
                self.reject("FnDecl", format!("parameter `{}` of `{}`", pname, n.name))
            } else {
                self.lty(pty)
            };
            match r {
                Ok(t) => params.push(t),
                // Recovery mode reports every parameter and the return type.
                Err(()) if self.recover => bad = true,
                Err(()) => return Err(()),
            }
        }
        let rt = n.extra_return_type.trim();
        let ret = if rt.is_empty() || rt == "void" {
            None
        } else {
            Some(self.lty(rt)?)
        };
        // A struct result is returned through a hidden pointer parameter.
        let total = n.params.len() + ret.as_ref().is_some_and(is_agg) as usize;
        if total > 8 {
            return self.reject(
                "FnDecl",
                format!("`{}` has {} parameters, at most 8 are supported", n.name, total),
            );
        }
        if bad {
            return Err(());
        }
        Ok((params, ret))
    }

    // ---------------------------------------------------------------- scopes

    fn lookup(&self, name: &str) -> Option<Binding> {
        for s in self.scopes.iter().rev() {
            if let Some(b) = s.get(name) {
                return Some(b.clone());
            }
        }
        None
    }

    fn bind(&mut self, name: &str, b: Binding) {
        if let Some(s) = self.scopes.last_mut() {
            s.insert(name.to_string(), b);
        }
    }

    /// A register variable of a scalar or pointer type, bound to `name`.
    fn new_lvar(&mut self, name: &str, t: LTy, mutable: bool) -> VarId {
        let id = self.hidden_var(name, t);
        self.bind(name, Binding::Var { id, mutable });
        id
    }

    /// A register variable no source name refers to.
    fn hidden_var(&mut self, name: &str, t: LTy) -> VarId {
        let id = self.vars.len() as VarId;
        let ty = reg_ty(&t).unwrap_or(Ty::Ptr);
        self.vars.push(Var {
            name: name.to_string(),
            ty,
        });
        self.ltys.push(t);
        id
    }

    /// Resolve a module-level `const`, evaluating it on first use.
    fn global(&mut self, name: &str) -> R<Option<Val>> {
        if let Some(v) = self.globals.get(name) {
            return Ok(Some(v.clone()));
        }
        let node = match self.const_nodes.get(name) {
            Some(n) => *n,
            None => return Ok(None),
        };
        if !self.resolving.insert(name.to_string()) {
            return self.reject("ConstDecl", format!("`{}` refers to itself", name));
        }
        let saved_line = self.line;
        let saved_scopes = std::mem::take(&mut self.scopes);
        self.see(node);
        let r = self.global_value(node);
        self.scopes = saved_scopes;
        self.line = saved_line;
        self.resolving.remove(name);
        let v = match r {
            Err(()) if self.recover => Val::Poison,
            r => r?,
        };
        self.globals.insert(name.to_string(), v.clone());
        Ok(Some(v))
    }

    fn global_value(&mut self, node: &Node) -> R<Val> {
        let init = match node.children.first() {
            Some(c) => c,
            None => return self.reject("ConstDecl", format!("`{}` has no value", node.name)),
        };
        let ann = node.extra_type.trim();
        let st = if !ann.is_empty() {
            match self.lty(ann)? {
                t @ LTy::Struct(_) => Some(t),
                t @ LTy::Arr(..) if holds_str(&t) => return self.const_array(init, &t),
                t @ LTy::Arr(..) => Some(t),
                LTy::Str => {
                    return match self.expr(init)? {
                        v @ (Val::S(..) | Val::Poison) => Ok(v),
                        _ => self.reject(
                            "ConstDecl",
                            format!("`{}` is a module-level str that is not a string literal", node.name),
                        ),
                    };
                }
                LTy::Ptr(..) => {
                    return self.reject("ConstDecl", format!("`{}` is a module-level pointer", node.name))
                }
                LTy::Slice(..) => {
                    return self.reject("ConstDecl(slice)", format!("`{}` is a module-level slice", node.name))
                }
                t @ LTy::Enum(..) => {
                    let v = self.expr_as(init, &t)?;
                    return match v {
                        Val::P(Expr { kind: ExprKind::Const(_), .. }, _) | Val::Poison => Ok(v),
                        _ => self.reject(
                            "ConstDecl",
                            format!("`{}` is not a compile-time enum value", node.name),
                        ),
                    };
                }
                LTy::S(_) => None,
            }
        } else if init.kind == NodeKind::ExprStructLit && !init.name.is_empty() {
            Some(self.lty(&init.name)?)
        } else {
            None
        };
        if let Some(t) = st {
            return self.rodata(init, t);
        }
        let v = self.expr(init)?;
        let v = if node.extra_type.trim().is_empty() {
            v
        } else {
            let ty = self.ty(&node.extra_type)?;
            Val::E(self.coerce(v, ty)?)
        };
        match &v {
            Val::Ct(_) | Val::S(..) | Val::A(..) | Val::Poison => Ok(v),
            Val::E(e) if matches!(e.kind, ExprKind::Const(_)) => Ok(v),
            Val::P(e, LTy::Enum(..)) if matches!(e.kind, ExprKind::Const(_)) => Ok(v),
            // Another struct constant: the same read-only bytes.
            Val::M(p) if matches!(p.addr.kind, ExprKind::Data(_)) => Ok(v),
            _ => self.reject(
                "ConstDecl",
                format!("`{}` is not a compile-time integer or bool", node.name),
            ),
        }
    }

    // ------------------------------------------------------------- functions

    fn begin_body(&mut self, body: &[Node]) {
        self.vars.clear();
        self.ltys.clear();
        self.slots.clear();
        self.sret = None;
        self.addr_taken.clear();
        scan_addr_taken(body, &mut self.addr_taken);
        self.scopes.clear();
        self.scopes.push(HashMap::new());
        self.loop_depth = 0;
    }

    fn function(&mut self, n: &Node) -> R<Func> {
        self.see(n);
        self.begin_body(&n.children);
        self.in_test = false;
        let (params, ret, poisoned) = {
            let s = &self.sigs[&n.name];
            (s.params.clone(), s.ret.clone(), s.poisoned)
        };
        self.ret = ret.clone();
        self.ret_poison = false;
        if poisoned {
            return self.poisoned_function(n);
        }
        // Parameters first (vars 0..), then the hidden result pointer, so
        // that they are exactly the first `nparams` variables.
        let mut ids = Vec::new();
        for (i, (pname, _)) in n.params.iter().enumerate() {
            let t = match &params[i] {
                t if is_agg(t) => LTy::Ptr(Box::new(t.clone()), false),
                t => t.clone(),
            };
            ids.push(self.hidden_var(pname, t));
        }
        if ret.as_ref().is_some_and(is_agg) {
            self.sret = Some(self.hidden_var("%sret", LTy::Ptr(Box::new(ret.clone().unwrap()), true)));
        }
        let nparams = self.vars.len();
        let mut body = Vec::new();
        for (i, (pname, _)) in n.params.iter().enumerate() {
            let var = Expr { ty: Ty::Ptr, kind: ExprKind::Var(ids[i]) };
            match &params[i] {
                // A struct or str argument is the caller's memory, read-only.
                t if is_agg(t) => {
                    let p = Place { addr: var, off: 0, ty: params[i].clone(), mutable: false, temp: None };
                    self.bind(pname, Binding::Mem(p));
                }
                t if self.addr_taken.contains(pname) => {
                    let ty = reg_ty(t).unwrap();
                    let k = self.new_slot(t)?;
                    let value = Expr { ty, kind: ExprKind::Var(ids[i]) };
                    body.push(Stmt::Store { addr: slot_expr(k), off: 0, value });
                    let p = Place { addr: slot_expr(k), off: 0, ty: t.clone(), mutable: false, temp: None };
                    self.bind(pname, Binding::Mem(p));
                }
                _ => self.bind(pname, Binding::Var { id: ids[i], mutable: true }),
            }
        }
        body.extend(self.stmts(&n.children)?);
        let ret = ret.map(|t| reg_ty(&t).unwrap_or(Ty::Ptr));
        let noreturn_site = if ret.is_some() {
            self.site(TrapKind::NoReturn, format!("end of fn {}", n.name), Ty::Bool)
        } else {
            0
        };
        Ok(Func {
            name: n.name.clone(),
            nparams,
            ret,
            vars: std::mem::take(&mut self.vars),
            slots: std::mem::take(&mut self.slots),
            body,
            line: n.line,
            is_test: false,
            is_invariant: false,
            noreturn_site,
        })
    }

    /// Recovery mode: the body of a fn whose signature was rejected, lowered
    /// only for what it reports. A parameter whose type is a scalar of the
    /// subset is a real variable; any other is poison, already reported.
    fn poisoned_function(&mut self, n: &Node) -> R<Func> {
        for (pname, pty) in &n.params {
            match Ty::from_name(pty.trim()) {
                Some(t) if !pname.starts_with("comptime ") => {
                    self.new_lvar(pname, LTy::S(t), true);
                }
                _ => self.bind(pname, Binding::Const(Val::Poison)),
            }
        }
        let rt = n.extra_return_type.trim();
        if !rt.is_empty() && rt != "void" {
            self.ret = Ty::from_name(rt).map(LTy::S);
            self.ret_poison = self.ret.is_none();
        }
        let _ = self.stmts(&n.children)?;
        Err(())
    }

    /// A `test` block, or an `invariant` block: both are a parameterless body
    /// of statements run once, and both use the test binding rule.
    fn test(&mut self, n: &Node, invariant: bool) -> R<Func> {
        let (k, what) = if invariant { ("InvariantBlock", "invariant") } else { ("TestBlock", "test") };
        self.block_body(n, k, what, invariant)
    }

    /// A `bench` block. t27c's Zig backend emits one as a plain
    /// `fn bench_<name>() void { ... }` that nothing calls, so `zig test`
    /// neither runs it nor counts it. Its body is lowered here with the test
    /// binding rule (the one t27c's `gen_bench_block` uses) and compiled by the
    /// caller, but never run: a construct t27b cannot lower in it rejects the
    /// file under that construct's own name.
    fn bench(&mut self, n: &Node) -> R<Func> {
        let mut f = self.block_body(n, "BenchBlock", "bench", false)?;
        f.is_test = false;
        Ok(f)
    }

    fn block_body(&mut self, n: &Node, k: &str, what: &str, invariant: bool) -> R<Func> {
        self.see(n);
        if n.line == 0 {
            if let Some(l) = self.src.and_then(|s| header_line(s, what, &n.name)) {
                self.line = l;
            }
        }
        if n.extra_field == "partial" {
            return self.reject(
                k,
                format!("{} `{}` was only partially parsed by the front-end", what, n.name),
            );
        }
        self.begin_body(&n.children);
        self.in_test = true;
        self.ret = None;
        self.ret_poison = false;
        self.test_assigns.clear();
        count_assigns(&n.children, &mut self.test_assigns);
        let body = self.stmts(&n.children)?;
        self.in_test = false;
        Ok(Func {
            name: n.name.clone(),
            nparams: 0,
            ret: None,
            vars: std::mem::take(&mut self.vars),
            slots: std::mem::take(&mut self.slots),
            body,
            line: n.line,
            is_test: true,
            is_invariant: invariant,
            noreturn_site: 0,
        })
    }

    // ------------------------------------------------------------ statements

    /// A nested block: its own scope.
    fn block(&mut self, n: &Node) -> R<Vec<Stmt>> {
        self.scopes.push(HashMap::new());
        let r = if n.kind == NodeKind::Module {
            self.stmts(&n.children)
        } else {
            self.stmts(std::slice::from_ref(n))
        };
        self.scopes.pop();
        r
    }

    fn stmts(&mut self, ns: &[Node]) -> R<Vec<Stmt>> {
        let mut out = Vec::new();
        for n in ns {
            if self.stmt(n, &mut out).is_err() {
                if !self.recover {
                    return Err(());
                }
                // Recovery mode: a name this statement would have declared is
                // poison from here on, so its uses are not reported again.
                self.poison_declared(n);
            }
        }
        Ok(out)
    }

    fn poison_declared(&mut self, n: &Node) {
        let name = match n.kind {
            NodeKind::StmtLocal => n.name.clone(),
            NodeKind::StmtAssign => match n.children.first() {
                Some(t) if t.kind == NodeKind::ExprIdentifier => t.name.clone(),
                _ => return,
            },
            _ => return,
        };
        let known = if n.kind == NodeKind::StmtLocal {
            self.scopes.last().map_or(false, |s| s.contains_key(&name))
        } else {
            self.lookup(&name).is_some()
        };
        if !name.is_empty() && !known {
            self.bind(&name, Binding::Const(Val::Poison));
        }
    }

    fn stmt(&mut self, n: &Node, out: &mut Vec<Stmt>) -> R<()> {
        self.see(n);
        match n.kind {
            NodeKind::StmtLocal => self.local(n, out),
            NodeKind::StmtAssign => self.assign(n, out),
            NodeKind::StmtIf => {
                if n.children.len() < 2 || n.children.len() > 3 {
                    return self.reject("StmtIf", "unexpected shape".into());
                }
                let cond = self.cond(&n.children[0])?;
                let then = self.block(&n.children[1])?;
                let els = match n.children.get(2) {
                    Some(e) => self.block(e)?,
                    None => Vec::new(),
                };
                out.push(Stmt::If { cond, then, els });
                Ok(())
            }
            NodeKind::StmtWhile => {
                if !n.name.is_empty() {
                    return self.reject("StmtWhile", format!("labelled loop `{}`", n.name));
                }
                let k = n.children.len();
                if !(2..=3).contains(&k) {
                    return self.reject("StmtWhile", "unexpected shape".into());
                }
                let cond = self.cond(&n.children[0])?;
                let body_node = &n.children[k - 1];
                if body_node.kind != NodeKind::Module || body_node.name != "body" {
                    return self.reject("StmtWhile", "unexpected body shape".into());
                }
                self.loop_depth += 1;
                let body = self.block(body_node);
                self.loop_depth -= 1;
                let body = body?;
                let step = if k == 3 {
                    let c = &n.children[1];
                    if c.kind != NodeKind::Module || c.name != "continue_expr" {
                        return self.reject("StmtWhile", "unexpected continue expression".into());
                    }
                    self.block(c)?
                } else {
                    Vec::new()
                };
                out.push(Stmt::While { cond, body, step });
                Ok(())
            }
            NodeKind::StmtFor => self.for_array(n, out),
            NodeKind::StmtBreak | NodeKind::StmtContinue => {
                let k = kind_name(n);
                if !n.name.is_empty() || !n.children.is_empty() {
                    return self.reject(&k, "labelled or valued break/continue".into());
                }
                if self.loop_depth == 0 {
                    return self.reject(&k, "outside a loop".into());
                }
                out.push(if n.kind == NodeKind::StmtBreak {
                    Stmt::Break
                } else {
                    Stmt::Continue
                });
                Ok(())
            }
            NodeKind::ExprReturn => {
                let v = match n.children.first() {
                    None => None,
                    Some(c) => Some(c),
                };
                if self.ret_poison {
                    // Recovery mode: the return type was already rejected.
                    if let Some(c) = v {
                        let _ = self.expr(c)?;
                    }
                    return Ok(());
                }
                match (self.ret.clone(), v) {
                    (None, None) => out.push(Stmt::Return(None)),
                    (Some(t), Some(c)) if is_agg(&t) => {
                        // Build the result in the caller's memory.
                        let sret = Expr { ty: Ty::Ptr, kind: ExprKind::Var(self.sret.unwrap()) };
                        let dst = Place { addr: sret.clone(), off: 0, ty: t, mutable: true, temp: None };
                        self.init(c, dst, true, out)?;
                        out.push(Stmt::Return(Some(sret)));
                    }
                    (Some(t), Some(c)) => {
                        let v = self.expr_as(c, &t)?;
                        let e = self.reg(v)?;
                        out.push(Stmt::Return(Some(e)));
                    }
                    (None, Some(_)) => {
                        return self.reject("ExprReturn", "value returned from a void fn or test".into())
                    }
                    (Some(_), None) => {
                        return self.reject("ExprReturn", "missing return value".into())
                    }
                }
                Ok(())
            }
            NodeKind::StmtExpr => match n.children.first() {
                Some(c) if c.kind == NodeKind::ExprCall => self.call_stmt(c, out),
                Some(c) if c.kind == NodeKind::ExprReturn => self.stmt(c, out),
                Some(c) => {
                    self.see(c);
                    let k = if c.kind == NodeKind::ExprUnary && !c.extra_op.is_empty() {
                        format!("ExprUnary({}) statement", c.extra_op.trim())
                    } else {
                        format!("{} statement", kind_name(c))
                    };
                    self.reject(&k, "expression statement".into())
                }
                None => self.reject("StmtExpr", "empty statement".into()),
            },
            NodeKind::ExprCall => self.call_stmt(n, out),
            NodeKind::Module if n.name.is_empty() || n.name == "block" => {
                let b = self.block(n)?;
                out.extend(b);
                Ok(())
            }
            _ => {
                let k = kind_name(n);
                self.reject(&k, String::new())
            }
        }
    }

    /// `for (a) |x| { ... }` over one array: a hidden index counts from 0 to
    /// the length, and `x` is the element it reaches, a constant. The array
    /// is evaluated once, before the loop; its elements are read as the loop
    /// reaches them.
    fn for_array(&mut self, n: &Node, out: &mut Vec<Stmt>) -> R<()> {
        if !n.name.is_empty() {
            return self.reject("StmtFor", format!("labelled loop `{}`", n.name));
        }
        if n.children.len() != 2 || n.params.len() != 1 {
            return self.reject("StmtFor", "more than one iterable or capture".into());
        }
        let (iter, body_node) = (&n.children[0], &n.children[1]);
        if body_node.kind != NodeKind::Module || body_node.name != "body" {
            return self.reject("StmtFor", "unexpected body shape".into());
        }
        if iter.kind == NodeKind::ExprBinary && iter.extra_op == ".." {
            return self.reject("StmtFor(range)", "`for` over a range".into());
        }
        let p = match self.expr(iter)? {
            Val::Poison => return Err(()),
            Val::M(p) if matches!(p.ty, LTy::Arr(..)) => p,
            Val::A(t, elems) => self.materialize(t, elems)?,
            Val::P(e, LTy::Ptr(inner, m)) if matches!(*inner, LTy::Arr(..)) => {
                Place { addr: e, off: 0, ty: *inner, mutable: m, temp: None }
            }
            Val::M(p) if matches!(p.ty, LTy::Str | LTy::Slice(..)) => p,
            v @ Val::S(..) => match self.coerce_to(v, &LTy::Str)? {
                Val::M(p) => p,
                _ => return Err(()),
            },
            v => {
                let d = self.val_desc(&v);
                return self.reject("StmtFor", format!("`for` over {}", d));
            }
        };
        let capture = n.params[0].0.trim().to_string();
        if capture.starts_with('*') || capture.contains(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            return self.reject("StmtFor", format!("capture `{}`", capture));
        }
        // The base address and the length: constants and the array for an
        // array; for a slice, both read from its header before the loop.
        let (elem, base, len) = match p.ty.clone() {
            LTy::Arr(elem, len) => {
                let mut base = addr_of(&p);
                if !pure_addr(&base) {
                    let h = self.hidden_var("%for_base", LTy::Ptr(Box::new(p.ty.clone()), false));
                    out.push(Stmt::Assign { var: h, value: base });
                    base = Expr { ty: Ty::Ptr, kind: ExprKind::Var(h) };
                }
                (*elem, base, Expr { ty: Ty::U64, kind: ExprKind::Const(len as i128) })
            }
            t => {
                let elem = match t {
                    LTy::Slice(elem, _) => *elem,
                    _ => LTy::S(Ty::U8),
                };
                let mut pin = Vec::new();
                let (hdr, off) = self.pin_header(&p, &mut pin)?;
                out.extend(pin);
                let hb = self.hidden_var("%for_base", LTy::Ptr(Box::new(elem.clone()), false));
                let hl = self.hidden_var("%for_len", LTy::S(Ty::U64));
                let ptr = Expr { ty: Ty::Ptr, kind: ExprKind::Load { addr: Box::new(hdr.clone()), off } };
                let n = Expr { ty: Ty::U64, kind: ExprKind::Load { addr: Box::new(hdr), off: off + 8 } };
                out.push(Stmt::Assign { var: hb, value: ptr });
                out.push(Stmt::Assign { var: hl, value: n });
                (elem, Expr { ty: Ty::Ptr, kind: ExprKind::Var(hb) }, Expr { ty: Ty::U64, kind: ExprKind::Var(hl) })
            }
        };
        let (esize, _) = self.size_align(&elem)?;
        let i = self.hidden_var("%for_i", LTy::S(Ty::U64));
        let var_i = Expr { ty: Ty::U64, kind: ExprKind::Var(i) };
        out.push(Stmt::Assign { var: i, value: Expr { ty: Ty::U64, kind: ExprKind::Const(0) } });
        let at = Expr {
            ty: Ty::Ptr,
            kind: ExprKind::Offset { base: Box::new(base), idx: Box::new(var_i.clone()), scale: esize },
        };
        let mut body = Vec::new();
        self.scopes.push(HashMap::new());
        if capture != "_" {
            match reg_ty(&elem) {
                Some(ty) => {
                    let value = Expr { ty, kind: ExprKind::Load { addr: Box::new(at), off: 0 } };
                    let x = self.new_lvar(&capture, elem.clone(), false);
                    body.push(Stmt::Assign { var: x, value });
                }
                None => {
                    let place = Place { addr: at, off: 0, ty: elem.clone(), mutable: false, temp: None };
                    self.bind(&capture, Binding::Mem(place));
                }
            }
        }
        self.loop_depth += 1;
        let r = self.stmts(&body_node.children);
        self.loop_depth -= 1;
        self.scopes.pop();
        body.extend(r?);
        let cond = Expr {
            ty: Ty::Bool,
            kind: ExprKind::Cmp {
                op: CmpOp::Lt,
                lhs: Box::new(var_i.clone()),
                rhs: Box::new(len),
            },
        };
        // i < len < 2^64, so i + 1 cannot wrap.
        let next = Expr {
            ty: Ty::U64,
            kind: ExprKind::Arith {
                op: ArithOp::AddW,
                lhs: Box::new(var_i),
                rhs: Box::new(Expr { ty: Ty::U64, kind: ExprKind::Const(1) }),
                site: 0,
            },
        };
        out.push(Stmt::While { cond, body, step: vec![Stmt::Assign { var: i, value: next }] });
        Ok(())
    }

    fn local(&mut self, n: &Node, out: &mut Vec<Stmt>) -> R<()> {
        let name = n.name.clone();
        if name.is_empty() || name.contains(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            return self.reject("StmtLocal", format!("binding `{}`", name));
        }
        let ann = n.extra_type.trim().to_string();
        let mutable = n.extra_mutable;
        let init = n.children.first().filter(|i| !is_undefined(i));
        if !ann.is_empty() {
            let t = self.lty(&ann)?;
            if is_agg(&t) || self.addr_taken.contains(&name) {
                // In memory; the name is bound only after its initializer.
                let k = self.new_slot(&t)?;
                let dst = Place { addr: slot_expr(k), off: 0, ty: t, mutable, temp: None };
                match init {
                    Some(i) => self.init(i, dst.clone(), true, out)?,
                    // A scalar with no value reads as zero, like a register.
                    None if !is_agg(&dst.ty) => {
                        let ty = reg_ty(&dst.ty).unwrap();
                        out.push(Stmt::Store { addr: slot_expr(k), off: 0, value: Expr { ty, kind: ExprKind::Const(0) } });
                    }
                    None => {}
                }
                self.bind(&name, Binding::Mem(dst));
                return Ok(());
            }
            let ty = reg_ty(&t).unwrap();
            let value = match init {
                Some(init) => {
                    let v = self.expr_as(init, &t)?;
                    if v.is_poison() {
                        // Recovery mode: the type is known, so keep the name.
                        self.new_lvar(&name, t, mutable);
                        return Err(());
                    }
                    self.reg(v)?
                }
                None => Expr {
                    ty,
                    kind: ExprKind::Const(0),
                },
            };
            let id = self.new_lvar(&name, t, mutable);
            out.push(Stmt::Assign { var: id, value });
            return Ok(());
        }
        let init = match init {
            Some(i) => i,
            None => return self.reject("StmtLocal", format!("`{}` has neither type nor value", name)),
        };
        let v = self.expr(init)?;
        match v {
            Val::Poison => self.bind(&name, Binding::Const(Val::Poison)),
            Val::Ct(c) => {
                if mutable {
                    return self.reject(
                        "StmtLocal",
                        format!("`var {}` initialised with an untyped integer needs a type", name),
                    );
                }
                self.bind(&name, Binding::Const(Val::Ct(c)));
            }
            v => self.bind_value(&name, v, mutable, out)?,
        }
        Ok(())
    }

    /// Bind `name` to a fresh variable holding `v` (not a comptime integer).
    fn bind_value(&mut self, name: &str, v: Val, mutable: bool, out: &mut Vec<Stmt>) -> R<()> {
        let (t, value) = match v {
            Val::Poison => return Err(()),
            Val::Ct(_) => return self.reject("StmtLocal", format!("`{}` needs a type", name)),
            // A string literal stays a compile-time value, like Zig's
            // `const s = "abc";`.
            Val::S(..) if !mutable => {
                self.bind(name, Binding::Const(v));
                return Ok(());
            }
            Val::S(..) => {
                let v = self.coerce_to(v, &LTy::Str)?;
                return self.bind_value(name, v, mutable, out);
            }
            // A compile-time array, likewise.
            Val::A(..) if !mutable => {
                self.bind(name, Binding::Const(v));
                return Ok(());
            }
            Val::A(t, elems) => {
                let p = self.materialize(t, elems)?;
                return self.bind_value(name, Val::M(p), mutable, out);
            }
            Val::E(e) => (LTy::S(e.ty), e),
            Val::P(e, t) => (t, e),
            Val::M(p) => {
                let k = match p.temp {
                    // A temporary nobody else sees becomes the variable.
                    Some(k) => {
                        match p.addr.kind {
                            ExprKind::Slot(_) => {}
                            ExprKind::Seq { stmts, .. } => out.extend(stmts),
                            _ => out.push(Stmt::Eval(p.addr)),
                        }
                        k
                    }
                    None => {
                        let k = self.new_slot(&p.ty)?;
                        let dst = Place { addr: slot_expr(k), off: 0, ty: p.ty.clone(), mutable: true, temp: None };
                        self.copy(&dst, p.clone(), out)?;
                        k
                    }
                };
                let dst = Place { addr: slot_expr(k), off: 0, ty: p.ty, mutable, temp: None };
                self.bind(name, Binding::Mem(dst));
                return Ok(());
            }
        };
        if self.addr_taken.contains(name) {
            let k = self.new_slot(&t)?;
            out.push(Stmt::Store { addr: slot_expr(k), off: 0, value });
            let dst = Place { addr: slot_expr(k), off: 0, ty: t, mutable, temp: None };
            self.bind(name, Binding::Mem(dst));
        } else {
            let id = self.new_lvar(name, t, mutable);
            out.push(Stmt::Assign { var: id, value });
        }
        Ok(())
    }

    fn assign(&mut self, n: &Node, out: &mut Vec<Stmt>) -> R<()> {
        if n.children.len() != 2 {
            return self.reject("StmtAssign", "unexpected shape".into());
        }
        let target = &n.children[0];
        let op = n.extra_op.as_str();
        if matches!(target.kind, NodeKind::ExprFieldAccess | NodeKind::ExprIndex) {
            let dst = self.lvalue(target)?;
            return self.store(dst, op, &n.children[1], out);
        }
        if target.kind != NodeKind::ExprIdentifier {
            let k = kind_name(target);
            return self.reject(&k, "assignment target".into());
        }
        let name = target.name.clone();
        match self.lookup(&name) {
            Some(Binding::Mem(dst)) => self.store(dst, op, &n.children[1], out),
            Some(Binding::Var { id, mutable }) if !matches!(self.ltys[id as usize], LTy::S(_)) => {
                if !mutable {
                    return self.reject("StmtAssign", format!("assignment to constant `{}`", name));
                }
                if !(op.is_empty() || op == "=") {
                    let d = if matches!(self.ltys[id as usize], LTy::Enum(..)) { "an enum" } else { "a pointer" };
                    return self.reject("StmtAssign", format!("`{}` on {}", op, d));
                }
                let t = self.ltys[id as usize].clone();
                let v = self.expr_as(&n.children[1], &t)?;
                let value = self.reg(v)?;
                out.push(Stmt::Assign { var: id, value });
                Ok(())
            }
            Some(Binding::Var { id, mutable }) => {
                if !mutable {
                    return self.reject("StmtAssign", format!("assignment to constant `{}`", name));
                }
                let ty = self.vars[id as usize].ty;
                let rhs = self.expr(&n.children[1])?;
                let v = if op.is_empty() || op == "=" {
                    rhs
                } else {
                    let bin = match op.strip_suffix('=') {
                        Some(b) if !b.is_empty() => b.to_string(),
                        _ => return self.reject("StmtAssign", format!("operator `{}`", op)),
                    };
                    let cur = Val::E(Expr {
                        ty,
                        kind: ExprKind::Var(id),
                    });
                    self.binary(&bin, cur, rhs)?
                };
                let value = self.coerce(v, ty)?;
                out.push(Stmt::Assign { var: id, value });
                Ok(())
            }
            // A name whose declaration was rejected: its cascade.
            Some(Binding::Const(Val::Poison)) => {
                let _ = self.expr(&n.children[1]);
                Err(())
            }
            Some(Binding::Const(_)) => {
                self.reject("StmtAssign", format!("assignment to constant `{}`", name))
            }
            None if self.in_test && (op.is_empty() || op == "=") && self.scopes.len() == 1 => {
                // Test-block binding form: the first plain assignment to a
                // fresh name declares it (t27c's Zig backend emits `const`, or
                // `var` when the test assigns the name again).
                let assigned = self.test_assigns.get(&name).copied().unwrap_or(0);
                let v = self.expr(&n.children[1])?;
                match v {
                    Val::Poison => self.bind(&name, Binding::Const(Val::Poison)),
                    Val::Ct(c) if assigned <= 1 => {
                        self.bind(&name, Binding::Const(Val::Ct(c)));
                    }
                    Val::Ct(_) => {
                        return self.reject(
                            "StmtAssign",
                            format!("test binding `{}` reassigned but its type is unknown", name),
                        )
                    }
                    v => self.bind_value(&name, v, assigned > 1, out)?,
                }
                Ok(())
            }
            None => self.reject("StmtAssign(undeclared)", format!("assignment to undeclared `{}`", name)),
        }
    }

    fn call_stmt(&mut self, c: &Node, out: &mut Vec<Stmt>) -> R<()> {
        self.see(c);
        match c.name.as_str() {
            "assert" => {
                if c.children.len() != 1 {
                    return self.reject("ExprCall(assert with message)", format!("assert with {} arguments", c.children.len()));
                }
                let cond = self.cond(&c.children[0])?;
                let site = self.site(TrapKind::Assert, "assert".into(), Ty::Bool);
                out.push(Stmt::Assert { cond, site });
                Ok(())
            }
            "assert_eq" => {
                if c.children.len() != 2 {
                    return self.reject(
                        "ExprCall",
                        format!("assert_eq with {} arguments", c.children.len()),
                    );
                }
                let (a, b) = self.operands(&c.children[0], &c.children[1])?;
                if a.is_poison() || b.is_poison() {
                    return Err(());
                }
                let (lhs, rhs) = match (a, b) {
                    (Val::P(x, LTy::Enum(i, _)), Val::P(y, LTy::Enum(j, _))) if i == j => (x, y),
                    (a @ Val::P(_, LTy::Enum(..)), b) | (a, b @ Val::P(_, LTy::Enum(..))) => {
                        let (x, y) = (self.val_desc(&a), self.val_desc(&b));
                        return self.reject("type mismatch", format!("assert_eq on {} and {}", x, y));
                    }
                    (Val::Ct(x), Val::Ct(y)) => {
                        let ty = if Ty::I64.fits(x) && Ty::I64.fits(y) {
                            Ty::I64
                        } else if Ty::U64.fits(x) && Ty::U64.fits(y) {
                            Ty::U64
                        } else {
                            return self.reject("ExprCall", "assert_eq on out-of-range literals".into());
                        };
                        (
                            Expr { ty, kind: ExprKind::Const(x) },
                            Expr { ty, kind: ExprKind::Const(y) },
                        )
                    }
                    (a, b) => self.peer(a, b, "assert_eq")?,
                };
                let site = self.site(TrapKind::AssertEq, "assert_eq".into(), lhs.ty);
                out.push(Stmt::AssertEq { lhs, rhs, site });
                Ok(())
            }
            _ => {
                let (call, _, _) = self.call(c, None)?;
                out.push(Stmt::Eval(call));
                Ok(())
            }
        }
    }

    /// A call: the `Call` expression (typed `Bool` for a void fn), the
    /// result type, and the temporary slot a struct result is built in when
    /// no `sret` destination is given.
    fn call(&mut self, c: &Node, sret: Option<Expr>) -> R<(Expr, Option<LTy>, Option<u32>)> {
        self.see(c);
        let (id, params, ret) = match self.sigs.get(&c.name) {
            Some(s) if s.poisoned => {
                // Recovery mode: report what the arguments contain, then drop
                // the call; its signature was already reported.
                for a in &c.children {
                    let _ = self.expr(a)?;
                }
                return Err(());
            }
            Some(s) => (s.id, s.params.clone(), s.ret.clone()),
            None if self.recover && self.poison_names.contains(&c.name) => {
                for a in &c.children {
                    let _ = self.expr(a)?;
                }
                return Err(());
            }
            // Normal mode: the callee's own rejection is the real blocker.
            None if self.poison_names.contains(&c.name) => {
                return self.reject(
                    "ExprCall(rejected fn)",
                    format!("call to `{}`, whose declaration was rejected", c.name),
                );
            }
            None => {
                let what = if c.name.starts_with('@') {
                    format!("ExprCall({})", c.name)
                } else if c.name == "assert" || c.name == "assert_eq" {
                    "ExprCall(assert in expression)".to_string()
                } else if matches!(c.name.as_str(), "Ok" | "Err" | "Some" | "None") {
                    "ExprCall(Result/Option constructor)".to_string()
                } else if c.name.starts_with("std.") {
                    "ExprCall(std.*)".to_string()
                } else if c.name.contains('.') {
                    "ExprCall(method)".to_string()
                } else if matches!(c.name.as_str(), "len" | "expect") {
                    format!("ExprCall({})", c.name)
                } else {
                    "ExprCall(undeclared fn)".to_string()
                };
                return self.reject(&what, format!("call to `{}`", c.name));
            }
        };
        if c.children.len() != params.len() {
            return self.reject(
                "ExprCall",
                format!(
                    "`{}` takes {} arguments, {} given",
                    c.name,
                    params.len(),
                    c.children.len()
                ),
            );
        }
        let mut args = Vec::new();
        for (i, a) in c.children.iter().enumerate() {
            let v = self.expr_as(a, &params[i])?;
            args.push(match v {
                // By reference; the callee never writes it.
                Val::M(p) => addr_of(&p),
                v => self.reg(v)?,
            });
        }
        let mut temp = None;
        let ty = match &ret {
            None => Ty::Bool,
            Some(t) if is_agg(t) => {
                let dst = match sret {
                    Some(d) => d,
                    None => {
                        let k = self.new_slot(ret.as_ref().unwrap())?;
                        temp = Some(k);
                        slot_expr(k)
                    }
                };
                args.push(dst);
                Ty::Ptr
            }
            Some(t) => reg_ty(t).unwrap(),
        };
        Ok((Expr { ty, kind: ExprKind::Call { func: id, args } }, ret, temp))
    }

    // ----------------------------------------------------------- expressions

    fn cond(&mut self, n: &Node) -> R<Expr> {
        let v = self.expr(n)?;
        match v {
            // Recovery mode: stand in a constant so the branches are lowered.
            Val::Poison => Ok(Expr { ty: Ty::Bool, kind: ExprKind::Const(0) }),
            Val::E(e) if e.ty == Ty::Bool => Ok(e),
            Val::E(e) => self.reject("condition", format!("expected bool, found {}", e.ty.name())),
            Val::Ct(_) => self.reject("condition", "expected bool, found an integer literal".into()),
            Val::P(_, ref t @ LTy::Enum(..)) => {
                let d = self.type_name(t);
                self.reject("condition", format!("expected bool, found {}", d))
            }
            Val::P(..) => self.reject("condition", "expected bool, found a pointer".into()),
            Val::M(ref p) if matches!(p.ty, LTy::Arr(..)) => self.reject("condition", "expected bool, found an array".into()),
            Val::A(..) => self.reject("condition", "expected bool, found an array".into()),
            Val::M(_) => self.reject("condition", "expected bool, found a struct".into()),
            Val::S(..) => self.reject("condition", "expected bool, found a string".into()),
        }
    }

    fn coerce(&mut self, v: Val, to: Ty) -> R<Expr> {
        match v {
            Val::Poison => Err(()),
            Val::Ct(c) => {
                if !to.is_int() {
                    return self.reject("type mismatch", "integer literal where bool is expected".into());
                }
                if !to.fits(c) {
                    return self.reject(
                        "literal out of range",
                        format!("{} does not fit in {}", c, to.name()),
                    );
                }
                Ok(Expr {
                    ty: to,
                    kind: ExprKind::Const(c),
                })
            }
            Val::E(e) => {
                if e.ty == to {
                    return Ok(e);
                }
                if to.can_widen_from(e.ty) {
                    if let ExprKind::Const(c) = e.kind {
                        return Ok(Expr {
                            ty: to,
                            kind: ExprKind::Const(c),
                        });
                    }
                    return Ok(Expr {
                        ty: to,
                        kind: ExprKind::Widen(Box::new(e)),
                    });
                }
                self.reject(
                    "type mismatch",
                    format!("expected {}, found {}", to.name(), e.ty.name()),
                )
            }
            Val::P(_, ref t @ LTy::Enum(..)) => {
                let d = self.type_name(t);
                self.reject("type mismatch", format!("expected {}, found {}", to.name(), d))
            }
            Val::P(..) => self.reject("type mismatch", format!("expected {}, found a pointer", to.name())),
            Val::M(ref p) if matches!(p.ty, LTy::Arr(..)) => {
                self.reject("type mismatch", format!("expected {}, found an array", to.name()))
            }
            Val::A(..) => self.reject("type mismatch", format!("expected {}, found an array", to.name())),
            Val::M(_) => self.reject("type mismatch", format!("expected {}, found a struct", to.name())),
            Val::S(..) => self.reject("type mismatch", format!("expected {}, found a string", to.name())),
        }
    }

    /// Bring two operands to one type (Zig peer type resolution, integers).
    fn peer(&mut self, a: Val, b: Val, what: &str) -> R<(Expr, Expr)> {
        if a.is_poison() || b.is_poison() {
            return Err(());
        }
        match (a, b) {
            (Val::E(x), Val::Ct(c)) => {
                let t = x.ty;
                let y = self.coerce(Val::Ct(c), t)?;
                Ok((x, y))
            }
            (Val::Ct(c), Val::E(y)) => {
                let t = y.ty;
                let x = self.coerce(Val::Ct(c), t)?;
                Ok((x, y))
            }
            (Val::E(x), Val::E(y)) => {
                if x.ty == y.ty {
                    return Ok((x, y));
                }
                if x.ty.can_widen_from(y.ty) {
                    let t = x.ty;
                    let y = self.coerce(Val::E(y), t)?;
                    return Ok((x, y));
                }
                if y.ty.can_widen_from(x.ty) {
                    let t = y.ty;
                    let x = self.coerce(Val::E(x), t)?;
                    return Ok((x, y));
                }
                self.reject(
                    "type mismatch",
                    format!("`{}` on {} and {}", what, x.ty.name(), y.ty.name()),
                )
            }
            (Val::Ct(_), Val::Ct(_)) => self.reject("type mismatch", "internal: two literals".into()),
            (Val::Poison, _) | (_, Val::Poison) => Err(()),
            (Val::A(..), _) | (_, Val::A(..)) => self.reject("type mismatch", format!("`{}` on an array", what)),
            (Val::M(p), _) | (_, Val::M(p)) if matches!(p.ty, LTy::Arr(..)) => {
                self.reject("type mismatch", format!("`{}` on an array", what))
            }
            (Val::M(_), _) | (_, Val::M(_)) => self.reject("type mismatch", format!("`{}` on a struct", what)),
            (Val::S(..), _) | (_, Val::S(..)) => self.reject("type mismatch", format!("`{}` on a string", what)),
            (Val::P(_, t @ LTy::Enum(..)), _) | (_, Val::P(_, t @ LTy::Enum(..))) => {
                let d = self.type_name(&t);
                self.reject("type mismatch", format!("`{}` on {}", what, d))
            }
            (Val::P(..), _) | (_, Val::P(..)) => self.reject("type mismatch", format!("`{}` on a pointer", what)),
        }
    }

    fn expr(&mut self, n: &Node) -> R<Val> {
        match self.expr_inner(n) {
            Err(()) if self.recover => Ok(Val::Poison),
            r => r,
        }
    }

    fn expr_inner(&mut self, n: &Node) -> R<Val> {
        self.see(n);
        match n.kind {
            NodeKind::ExprLiteral => self.literal(n),
            NodeKind::ExprIdentifier => {
                let name = n.name.as_str();
                if name == "true" || name == "false" {
                    return Ok(Val::E(Expr {
                        ty: Ty::Bool,
                        kind: ExprKind::Const((name == "true") as i128),
                    }));
                }
                if let Some((e, v)) = name.split_once("::") {
                    if self.lookup(e).is_none() && self.enum_nodes.contains_key(e) {
                        let Some(id) = self.enum_id(e)? else { unreachable!() };
                        return self.enum_value(id, v);
                    }
                }
                match self.lookup(name) {
                    Some(Binding::Var { id, .. }) => {
                        let e = Expr { ty: self.vars[id as usize].ty, kind: ExprKind::Var(id) };
                        Ok(val_of(e, &self.ltys[id as usize]))
                    }
                    Some(Binding::Const(v)) => Ok(v),
                    Some(Binding::Mem(p)) => self.place_value(p),
                    None => match self.global(name)? {
                        Some(v) => Ok(v),
                        None => self.unknown_name(name),
                    },
                }
            }
            NodeKind::ExprBinary => {
                if n.children.len() != 2 {
                    return self.reject("ExprBinary", "unexpected shape".into());
                }
                let op = n.extra_op.clone();
                if op == "and" || op == "or" || op == "&&" || op == "||" {
                    let a = self.cond(&n.children[0])?;
                    let b = self.cond(&n.children[1])?;
                    let and = op == "and" || op == "&&";
                    if let (ExprKind::Const(x), ExprKind::Const(y)) = (&a.kind, &b.kind) {
                        let r = if and { *x & *y } else { *x | *y };
                        return Ok(Val::E(Expr { ty: Ty::Bool, kind: ExprKind::Const(r) }));
                    }
                    let kind = if and {
                        ExprKind::And(Box::new(a), Box::new(b))
                    } else {
                        ExprKind::Or(Box::new(a), Box::new(b))
                    };
                    return Ok(Val::E(Expr { ty: Ty::Bool, kind }));
                }
                let (x, y) = (&n.children[0], &n.children[1]);
                let lit = |n: &Node| n.kind == NodeKind::ExprEnumValue;
                let ordered = (self.names_variant(x) || self.names_variant(y)) && !lit(x) && !lit(y);
                let (a, b) = self.operands(x, y)?;
                if let Some(v) = self.enum_compare(&op, &a, &b, ordered)? {
                    return Ok(v);
                }
                self.binary(&op, a, b)
            }
            NodeKind::ExprEnumValue => {
                self.reject("ExprEnumValue", format!("enum literal `.{}` with no result type", n.name))
            }
            NodeKind::ExprUnary => {
                if n.children.len() != 1 {
                    return self.reject("ExprUnary", "unexpected shape".into());
                }
                let op = n.extra_op.clone();
                if op == "&" {
                    let p = self.lvalue(&n.children[0])?;
                    let t = LTy::Ptr(Box::new(p.ty.clone()), p.mutable);
                    return Ok(Val::P(addr_of(&p), t));
                }
                let v = self.expr(&n.children[0])?;
                self.unary(&op, v)
            }
            NodeKind::ExprCall if n.name == "@intFromEnum" => self.int_from_enum(n, TagUse::Value),
            NodeKind::ExprCall if n.name == "@enumFromInt" => {
                self.reject("ExprCall(@enumFromInt)", "with no enum result type".into())
            }
            // `@as(T, x)`: `x` coerced to `T`.
            NodeKind::ExprCall if n.name == "@as" && n.children.len() == 2 && n.children[0].kind == NodeKind::ExprIdentifier => {
                let t = self.lty(&n.children[0].name)?;
                self.expr_as(&n.children[1], &t)
            }
            NodeKind::ExprCall => {
                let (call, ret, temp) = self.call(n, None)?;
                match ret {
                    Some(t) if is_agg(&t) => {
                        Ok(Val::M(Place { addr: call, off: 0, ty: t, mutable: false, temp }))
                    }
                    Some(t) => Ok(val_of(call, &t)),
                    None => self.reject("ExprCall", format!("void fn `{}` used as a value", n.name)),
                }
            }
            NodeKind::ExprCast => {
                if n.children.len() != 1 {
                    return self.reject("ExprCast", "unexpected shape".into());
                }
                // The operand first, so that in recovery mode an unsupported
                // operand and an unsupported target type are both named.
                let v = self.expr(&n.children[0])?;
                let to = self.ty(n.extra_type.trim())?;
                self.cast(v, to)
            }
            NodeKind::ExprFieldAccess => {
                // `.len` of a compile-time string is a constant.
                if n.name == "len"
                    && n.children.len() == 1
                    && matches!(n.children[0].kind, NodeKind::ExprIdentifier | NodeKind::ExprLiteral)
                {
                    if let Some(len) = self.peek_string(&n.children[0])? {
                        return Ok(Val::E(Expr { ty: Ty::U64, kind: ExprKind::Const(len as i128) }));
                    }
                }
                match self.member(n)? {
                    Ok(p) => self.place_value(p),
                    Err(v) => Ok(v),
                }
            }
            NodeKind::ExprIndex => match self.index(n)? {
                Ok(p) => self.place_value(p),
                Err(v) => Ok(v),
            },
            NodeKind::ExprArrayLiteral => {
                self.reject("ExprArrayLiteral", "array literal with no result type".into())
            }
            NodeKind::ExprStructLit => {
                if n.name.is_empty() {
                    return self.reject("ExprStructLit", "anonymous `.{}` literal with no result type".into());
                }
                let t = self.lty(&n.name)?;
                self.struct_temp(n, t)
            }
            _ => {
                let k = kind_name(n);
                self.reject(&k, String::new())
            }
        }
    }

    /// A literal or a name, evaluated only if it is a compile-time string:
    /// nothing is reported or emitted otherwise.
    fn peek_string(&mut self, n: &Node) -> R<Option<u64>> {
        if n.kind == NodeKind::ExprLiteral {
            return Ok((n.extra_kind == "string").then(|| n.value.len() as u64));
        }
        match self.lookup(&n.name) {
            Some(Binding::Const(Val::S(_, len))) => Ok(Some(len)),
            Some(_) => Ok(None),
            // Only a constant initialised with a string literal is evaluated
            // here; any rejection it has is its own.
            None if self.const_nodes.get(&n.name).is_some_and(|c| {
                c.children.first().is_some_and(|i| i.kind == NodeKind::ExprLiteral && i.extra_kind == "string")
            }) =>
            {
                match self.global(&n.name)? {
                    Some(Val::S(_, len)) => Ok(Some(len)),
                    _ => Ok(None),
                }
            }
            None => Ok(None),
        }
    }

    /// `v as to`. Lossless conversions (bool to an integer as 0 / 1
    /// included) are a `Widen`. A narrowing between two unsigned types
    /// truncates, like Zig's `@truncate`; every other one is checked, like
    /// `@intCast`, and traps when the value is outside `to`. In Wrap mode every
    /// narrowing truncates, as a C cast does.
    fn cast(&mut self, v: Val, to: Ty) -> R<Val> {
        let e = match v {
            Val::Poison => return Err(()),
            Val::Ct(c) if to.is_int() => return Ok(Val::E(self.coerce(Val::Ct(c), to)?)),
            Val::Ct(_) => return self.reject("ExprCast(to bool)", "integer literal as bool".into()),
            Val::E(e) => e,
            Val::P(_, t @ LTy::Enum(..)) => {
                let d = self.type_name(&t);
                return self.reject("ExprCast", format!("enum `{}` as {}", d, to.name()));
            }
            Val::P(..) => return self.reject("ExprCast", format!("pointer as {}", to.name())),
            Val::M(_) | Val::A(..) => return self.reject("ExprCast", format!("struct or array as {}", to.name())),
            Val::S(..) => return self.reject("ExprCast", format!("string as {}", to.name())),
        };
        let from = e.ty;
        if from == to {
            return Ok(Val::E(e));
        }
        if to == Ty::Bool {
            return self.reject("ExprCast(to bool)", format!("{} as bool", from.name()));
        }
        if from == Ty::Bool || to.can_widen_from(from) {
            if let ExprKind::Const(c) = e.kind {
                return Ok(Val::E(Expr { ty: to, kind: ExprKind::Const(c) }));
            }
            return Ok(Val::E(Expr { ty: to, kind: ExprKind::Widen(Box::new(e)) }));
        }
        let truncate = self.mode == OverflowMode::Wrap || (!from.signed() && !to.signed());
        if let ExprKind::Const(c) = e.kind {
            if truncate {
                return Ok(Val::E(Expr { ty: to, kind: ExprKind::Const(to.wrap(c)) }));
            }
            if to.fits(c) {
                return Ok(Val::E(Expr { ty: to, kind: ExprKind::Const(c) }));
            }
        }
        let site = if truncate {
            0
        } else {
            self.site(TrapKind::Cast, format!("{} as {}", from.name(), to.name()), to)
        };
        Ok(Val::E(Expr { ty: to, kind: ExprKind::Cast { arg: Box::new(e), site } }))
    }

    /// A name that is neither in scope nor a module-level constant.
    fn unknown_name<T>(&mut self, name: &str) -> R<T> {
        if self.poison_names.contains(name) {
            if self.recover {
                return Err(());
            }
            return self.reject(
                "ExprIdentifier(rejected decl)",
                format!("`{}`, whose declaration was rejected", name),
            );
        }
        // `E::V` of an enum this file rejected: its cascade.
        if self.recover && name.split_once("::").map_or(false, |(e, _)| self.poison_names.contains(e)) {
            return Err(());
        }
        let what = if name == "null" {
            "ExprLiteral(null)"
        } else if name.contains("::") {
            "ExprIdentifier(E::V)"
        } else if Ty::from_name(name).is_some()
            || name.starts_with('[')
            || matches!(name, "f32" | "f64" | "usize" | "isize" | "type")
        {
            "ExprIdentifier(type as value)"
        } else {
            "ExprIdentifier(undeclared)"
        };
        self.reject(what, format!("unknown name `{}`", name))
    }

    fn literal(&mut self, n: &Node) -> R<Val> {
        // The t27c lexer strips the quotes and unescapes, so the node holds
        // the string's own bytes (not trimmed: spaces are part of it).
        if n.extra_kind == "string" {
            return Ok(self.string(n.value.as_bytes()));
        }
        let s = n.value.trim();
        if s == "true" || s == "false" {
            return Ok(Val::E(Expr {
                ty: Ty::Bool,
                kind: ExprKind::Const((s == "true") as i128),
            }));
        }
        let v = match parse_int(s) {
            Some(v) => v,
            None => {
                // The t27c parser strips a string literal's quotes and marks
                // the node with extra_kind "string" instead.
                let what = if n.extra_kind == "string" || s.starts_with('"') {
                    "string literal"
                } else if s.starts_with('\'') {
                    "char literal"
                } else if s.contains('.') || (s.contains(['e', 'E']) && !s.starts_with("0x")) {
                    "float literal"
                } else if s.starts_with('-') && parse_int(&s[1..]).is_some() {
                    "negative literal"
                } else {
                    "literal"
                };
                return self.reject(&format!("ExprLiteral({})", what), format!("`{}`", s));
            }
        };
        let suffix = n.extra_type.trim();
        if suffix.is_empty() {
            Ok(Val::Ct(v))
        } else {
            let ty = self.ty(suffix)?;
            Ok(Val::E(self.coerce(Val::Ct(v), ty)?))
        }
    }

    fn unary(&mut self, op: &str, v: Val) -> R<Val> {
        if v.is_poison() {
            return Err(());
        }
        match (op, v) {
            ("-", Val::Ct(c)) => Ok(Val::Ct(-c)),
            ("-", Val::E(e)) => {
                if !e.ty.is_int() || !e.ty.signed() {
                    return self.reject(
                        "ExprUnary(-)",
                        format!("negation of {}", e.ty.name()),
                    );
                }
                let ty = e.ty;
                let zero = Expr { ty, kind: ExprKind::Const(0) };
                let op = if self.mode == OverflowMode::Trap { ArithOp::Sub } else { ArithOp::SubW };
                let site = if op == ArithOp::Sub {
                    self.site(TrapKind::Overflow, format!("- on {}", ty.name()), ty)
                } else {
                    0
                };
                Ok(Val::E(Expr {
                    ty,
                    kind: ExprKind::Arith { op, lhs: Box::new(zero), rhs: Box::new(e), site },
                }))
            }
            ("!", Val::E(e)) if e.ty == Ty::Bool => {
                if let ExprKind::Const(c) = e.kind {
                    return Ok(Val::E(Expr { ty: Ty::Bool, kind: ExprKind::Const(1 - c) }));
                }
                Ok(Val::E(Expr { ty: Ty::Bool, kind: ExprKind::Not(Box::new(e)) }))
            }
            ("~", Val::E(e)) if e.ty.is_int() => {
                let ty = e.ty;
                Ok(Val::E(Expr { ty, kind: ExprKind::BitNot(Box::new(e)) }))
            }
            (op, Val::E(e)) => self.reject(
                &format!("ExprUnary({})", op),
                format!("operand of type {}", e.ty.name()),
            ),
            (op, Val::Ct(_)) => self.reject(
                &format!("ExprUnary({})", op),
                "on an untyped integer literal".into(),
            ),
            (op, _) => self.reject(&format!("ExprUnary({})", op), "operand is a pointer, a struct or a string".into()),
        }
    }

    fn binary(&mut self, op: &str, a: Val, b: Val) -> R<Val> {
        if a.is_poison() || b.is_poison() {
            return Err(());
        }
        if self.is_str(&a) || self.is_str(&b) {
            return match op {
                "==" | "!=" => self.str_eq(op == "!=", a, b),
                _ => self.reject(&format!("ExprBinary({})", op), "on a string".into()),
            };
        }
        if matches!(a, Val::P(..) | Val::M(_) | Val::A(..)) || matches!(b, Val::P(..) | Val::M(_) | Val::A(..)) {
            let arr = |v: &Val| matches!(v, Val::A(..)) || matches!(v, Val::M(p) if matches!(p.ty, LTy::Arr(..)));
            let what = if arr(&a) || arr(&b) {
                "an array"
            } else if matches!(a, Val::M(_)) || matches!(b, Val::M(_)) {
                "a struct"
            } else {
                "a pointer"
            };
            return self.reject("type mismatch", format!("`{}` on {}", op, what));
        }
        let cmp = match op {
            "==" => Some(CmpOp::Eq),
            "!=" => Some(CmpOp::Ne),
            "<" => Some(CmpOp::Lt),
            "<=" => Some(CmpOp::Le),
            ">" => Some(CmpOp::Gt),
            ">=" => Some(CmpOp::Ge),
            _ => None,
        };
        if let Some(c) = cmp {
            return self.compare(c, a, b);
        }
        let trap = self.mode == OverflowMode::Trap;
        let aop = match op {
            "+" => if trap { ArithOp::Add } else { ArithOp::AddW },
            "-" => if trap { ArithOp::Sub } else { ArithOp::SubW },
            "*" => if trap { ArithOp::Mul } else { ArithOp::MulW },
            "+%" => ArithOp::AddW,
            "-%" => ArithOp::SubW,
            "*%" => ArithOp::MulW,
            "/" => if trap { ArithOp::Div } else { ArithOp::DivW },
            "%" => ArithOp::Rem,
            "&" => ArithOp::And,
            "|" => ArithOp::Or,
            "^" => ArithOp::Xor,
            "<<" => if trap { ArithOp::Shl } else { ArithOp::ShlW },
            ">>" => if trap { ArithOp::Shr } else { ArithOp::ShrW },
            _ => return self.reject(&format!("ExprBinary({})", op), String::new()),
        };
        if aop.is_shift() {
            return self.shift(aop, a, b);
        }
        if let (Val::Ct(x), Val::Ct(y)) = (&a, &b) {
            return self.fold(aop, *x, *y).map(Val::Ct);
        }
        let (x, y) = self.peer(a, b, op)?;
        let ty = x.ty;
        let bitwise = matches!(aop, ArithOp::And | ArithOp::Or | ArithOp::Xor);
        if !ty.is_int() && !bitwise {
            return self.reject(&format!("ExprBinary({})", op), "on bool".into());
        }
        let site = match aop {
            ArithOp::Add | ArithOp::Sub | ArithOp::Mul => {
                self.site(TrapKind::Overflow, format!("{} on {}", op, ty.name()), ty)
            }
            ArithOp::Div => {
                // Two consecutive sites: divisor zero, then MIN / -1.
                let s = self.site(TrapKind::DivZero, format!("/ on {}", ty.name()), ty);
                self.site(TrapKind::Overflow, format!("/ on {}", ty.name()), ty);
                s
            }
            ArithOp::DivW | ArithOp::Rem => {
                self.site(TrapKind::DivZero, format!("{} on {}", op, ty.name()), ty)
            }
            _ => 0,
        };
        Ok(Val::E(Expr {
            ty,
            kind: ExprKind::Arith { op: aop, lhs: Box::new(x), rhs: Box::new(y), site },
        }))
    }

    fn fold(&mut self, op: ArithOp, x: i128, y: i128) -> R<i128> {
        let r = match op {
            ArithOp::Add | ArithOp::AddW => x.checked_add(y),
            ArithOp::Sub | ArithOp::SubW => x.checked_sub(y),
            ArithOp::Mul | ArithOp::MulW => x.checked_mul(y),
            ArithOp::Div | ArithOp::DivW | ArithOp::Rem => {
                if y == 0 {
                    return self.reject("ExprBinary", "constant division by zero".into());
                }
                if op == ArithOp::Rem { x.checked_rem(y) } else { x.checked_div(y) }
            }
            ArithOp::And => Some(x & y),
            ArithOp::Or => Some(x | y),
            ArithOp::Xor => Some(x ^ y),
            _ => None,
        };
        match r {
            Some(v) => Ok(v),
            None => self.reject("ExprBinary", "constant expression overflows 128 bits".into()),
        }
    }

    fn shift(&mut self, op: ArithOp, a: Val, b: Val) -> R<Val> {
        let left = matches!(op, ArithOp::Shl | ArithOp::ShlW);
        match (a, b) {
            (Val::Ct(x), Val::Ct(y)) => {
                if !(0..127).contains(&y) {
                    return self.reject("ExprBinary", format!("constant shift by {}", y));
                }
                if left {
                    let r = x << y;
                    if (r >> y) != x {
                        return self.reject("ExprBinary", "constant shift overflows 128 bits".into());
                    }
                    Ok(Val::Ct(r))
                } else {
                    Ok(Val::Ct(x >> y))
                }
            }
            (Val::Ct(_), _) => self.reject(
                "ExprBinary(<< >>)",
                "untyped literal shifted by a runtime amount".into(),
            ),
            (Val::E(x), amt) => {
                let ty = x.ty;
                if !ty.is_int() {
                    return self.reject("ExprBinary(<< >>)", format!("shift of {}", ty.name()));
                }
                let bits = ty.bits() as i128;
                match amt {
                    Val::Ct(c) => {
                        let c = if (0..bits).contains(&c) {
                            c
                        } else if self.mode == OverflowMode::Wrap {
                            c & (bits - 1)
                        } else {
                            return self.reject(
                                "ExprBinary(<< >>)",
                                format!("shift amount {} out of range for {}", c, ty.name()),
                            );
                        };
                        let wop = if left { ArithOp::ShlW } else { ArithOp::ShrW };
                        let amt = Expr { ty: Ty::U32, kind: ExprKind::Const(c) };
                        Ok(Val::E(Expr {
                            ty,
                            kind: ExprKind::Arith { op: wop, lhs: Box::new(x), rhs: Box::new(amt), site: 0 },
                        }))
                    }
                    Val::E(y) => {
                        if !y.ty.is_int() {
                            return self.reject("ExprBinary(<< >>)", "bool shift amount".into());
                        }
                        let site = if matches!(op, ArithOp::Shl | ArithOp::Shr) {
                            self.site(TrapKind::ShiftRange, format!("{} on {}", op.symbol(), ty.name()), ty)
                        } else {
                            0
                        };
                        Ok(Val::E(Expr {
                            ty,
                            kind: ExprKind::Arith { op, lhs: Box::new(x), rhs: Box::new(y), site },
                        }))
                    }
                    _ => self.reject("ExprBinary(<< >>)", "shift amount is a pointer or a struct".into()),
                }
            }
            _ => self.reject("ExprBinary(<< >>)", "shift of a pointer or a struct".into()),
        }
    }

    fn compare(&mut self, op: CmpOp, a: Val, b: Val) -> R<Val> {
        if let (Val::Ct(x), Val::Ct(y)) = (&a, &b) {
            return Ok(Val::E(Expr {
                ty: Ty::Bool,
                kind: ExprKind::Const(op.holds(*x, *y) as i128),
            }));
        }
        let (x, y) = self.peer(a, b, op.symbol())?;
        if x.ty == Ty::Bool && !matches!(op, CmpOp::Eq | CmpOp::Ne) {
            return self.reject(&format!("ExprBinary({})", op.symbol()), "ordering on bool".into());
        }
        Ok(Val::E(Expr {
            ty: Ty::Bool,
            kind: ExprKind::Cmp { op, lhs: Box::new(x), rhs: Box::new(y) },
        }))
    }

    // ---------------------------------------------------------------- memory

    /// A source type: `*T`, `*const T`, a scalar, or a struct (laid out).
    fn lty(&mut self, name: &str) -> R<LTy> {
        self.lty_in(name, true)
    }

    /// `by_value`: whether a struct's layout is needed now. The pointee of a
    /// pointer is only named, so a struct may point to itself.
    fn lty_in(&mut self, name: &str, by_value: bool) -> R<LTy> {
        let t = name.trim();
        if let Some(rest) = t.strip_prefix('*') {
            let rest = rest.trim_start();
            let (inner, mutable) = match rest.strip_prefix("const ") {
                Some(r) => (r, false),
                None => (rest, true),
            };
            let inner = self.lty_in(inner, false)?;
            return Ok(LTy::Ptr(Box::new(inner), mutable));
        }
        if let Some(ty) = Ty::from_name(t) {
            return Ok(LTy::S(ty));
        }
        // t27c's Zig backend spells all four `[]const u8`.
        if matches!(t, "str" | "&str" | "string" | "[]const u8") {
            return Ok(LTy::Str);
        }
        // `[]T`, `[]const T`. The elements are only pointed to, so a struct
        // may hold a slice of itself.
        if let Some(rest) = t.strip_prefix("[]") {
            let rest = rest.trim_start();
            let (inner, mutable) = match rest.strip_prefix("const ") {
                Some(r) => (r.trim(), false),
                None => (rest, true),
            };
            if !inner.is_empty() {
                let inner = self.lty_in(inner, false)?;
                if inner == LTy::S(Ty::U8) && !mutable {
                    return Ok(LTy::Str);
                }
                return Ok(LTy::Slice(Box::new(inner), mutable));
            }
        }
        if self.enum_nodes.contains_key(t) {
            let Some(id) = self.enum_id(t)? else { unreachable!() };
            return Ok(LTy::Enum(id, self.enums[id as usize].tag));
        }
        if self.struct_nodes.contains_key(t) {
            let id = self.struct_id(t);
            if by_value {
                self.layout(id)?;
            }
            return Ok(LTy::Struct(id));
        }
        // `[N]T`, N a literal or the name of a compile-time integer.
        if let Some((len, elem)) = t.strip_prefix('[').and_then(|r| r.split_once(']')) {
            let (len, elem) = (len.trim(), elem.trim());
            if !len.is_empty() && !elem.is_empty() {
                let n = self.array_len(t, len)?;
                let inner = self.lty_in(elem, by_value)?;
                return Ok(LTy::Arr(Box::new(inner), n));
            }
        }
        let (construct, detail) = self.type_construct(t);
        self.reject(&construct, detail)
    }

    /// The length of array type `t`, spelled `len`.
    fn array_len(&mut self, t: &str, len: &str) -> R<u32> {
        let v = if let Some(c) = parse_int(len) {
            Some(c)
        } else if len.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !len.starts_with(|c: char| c.is_ascii_digit()) && len != "_" {
            let v = match self.lookup(len) {
                Some(Binding::Const(v)) => Some(v),
                Some(_) => None,
                None => self.global(len)?,
            };
            match v {
                Some(Val::Poison) => return Err(()),
                Some(Val::Ct(c)) => Some(c),
                Some(Val::E(Expr { kind: ExprKind::Const(c), ty })) if ty.is_int() => Some(c),
                _ => None,
            }
        } else {
            None
        };
        match v {
            Some(c) if (0..=u32::MAX as i128).contains(&c) => Ok(c as u32),
            Some(c) => self.reject("type [N]T", format!("`{}`: length {} out of range", t, c)),
            None => self.reject("type [N]T", format!("`{}`: length `{}` is not a compile-time integer", t, len)),
        }
    }

    fn struct_id(&mut self, name: &str) -> u32 {
        if let Some(&id) = self.struct_ids.get(name) {
            return id;
        }
        let id = self.structs.len() as u32;
        self.structs.push(StructDef { name: name.to_string(), fields: Vec::new(), size: None, align: 1, fail: None });
        self.struct_ids.insert(name.to_string(), id);
        id
    }

    /// The id of enum `name`, building it on first use; None when no enum
    /// of that name is declared. A refused declaration is reported where it
    /// is first built and, outside recovery mode, again at every later use.
    fn enum_id(&mut self, name: &str) -> R<Option<u32>> {
        if let Some(&id) = self.enum_ids.get(name) {
            return Ok(Some(id));
        }
        if let Some(r) = self.enum_fail.get(name).cloned() {
            if !self.recover {
                self.errors.push(Reject { line: self.line, ..r });
            }
            return Err(());
        }
        let Some(&node) = self.enum_nodes.get(name) else { return Ok(None) };
        let key = format!("enum {}", name);
        if !self.resolving.insert(key.clone()) {
            return self.reject("EnumDecl", format!("a tag of `{}` refers to `{}` itself", name, name));
        }
        let saved = self.line;
        let saved_scopes = std::mem::take(&mut self.scopes);
        self.line = 0;
        self.see(node);
        if node.line == 0 {
            if let Some(l) = self.src.and_then(|s| decl_line(s, name)) {
                self.line = l;
            }
        }
        let nerr = self.errors.len();
        let r = self.enum_build(node);
        self.scopes = saved_scopes;
        self.line = saved;
        self.resolving.remove(&key);
        match r {
            Ok(def) => {
                let id = self.enums.len() as u32;
                self.enums.push(def);
                self.enum_ids.insert(name.to_string(), id);
                Ok(Some(id))
            }
            Err(()) => {
                if let Some(e) = self.errors.get(nerr).cloned() {
                    self.enum_fail.insert(name.to_string(), e);
                }
                Err(())
            }
        }
    }

    /// Tags as t27c's Zig backend declares them: `enum(T)` when a tag type
    /// is written, `enum(i32)` when it is not but some variant has a value,
    /// plain `enum` (Zig's smallest unsigned tag, values 0, 1, ...) otherwise.
    fn enum_build(&mut self, node: &Node) -> R<EnumDef> {
        let name = node.name.clone();
        for v in &node.children {
            if v.kind != NodeKind::EnumVariant {
                return self.reject("EnumDecl", format!("`{}` member of kind {}", name, kind_name(v)));
            }
        }
        // t27c reads every word in the braces as a variant, so a method or a
        // declaration inside the enum shows up as variants named after its
        // keywords.
        if let Some(v) = node.children.iter().find(|v| matches!(v.name.as_str(), "fn" | "pub" | "const" | "var")) {
            return self.reject(
                "EnumDecl(method)",
                format!("`{}` declares something inside it (`{}`); only plain enums are supported", name, v.name),
            );
        }
        if node.children.iter().any(|v| v.name == "_") {
            return self.reject(
                "EnumDecl(non-exhaustive)",
                format!("`{}` has a `_` variant; only exhaustive enums are supported", name),
            );
        }
        if node.children.is_empty() {
            return self.reject("EnumDecl", format!("`{}` has no variants", name));
        }
        if let Some(v) = node.children.iter().find(|v| v.name.starts_with(|c: char| c.is_ascii_digit())) {
            return self.reject("EnumDecl", format!("`{}`: `{}` is not a variant name", name, v.name));
        }
        let n = node.children.len();
        let ann = node.extra_type.trim();
        let valued = node.children.iter().any(|v| !v.value.is_empty());
        let (tag, bits) = if !ann.is_empty() {
            match Ty::from_name(ann) {
                Some(t) if t.is_int() => (t, t.bits()),
                _ => {
                    return self.reject(
                        "EnumDecl(tag type)",
                        format!("`{}` has tag type `{}`", name, ann),
                    )
                }
            }
        } else if valued {
            (Ty::I32, 32)
        } else {
            let mut bits = 0u32;
            while (1u128 << bits) < n as u128 {
                bits += 1;
            }
            let tag = match bits {
                0..=8 => Ty::U8,
                9..=16 => Ty::U16,
                17..=32 => Ty::U32,
                _ => Ty::U64,
            };
            (tag, bits)
        };
        let mut variants: Vec<(String, i128)> = Vec::new();
        let mut next: i128 = 0;
        for v in &node.children {
            let val = if v.value.is_empty() {
                next
            } else {
                let (neg, digits) = match v.value.strip_prefix('-') {
                    Some(d) => (true, d),
                    None => (false, v.value.as_str()),
                };
                let c = if let Some(c) = parse_int(digits) {
                    c
                } else {
                    match self.global(digits)? {
                        Some(Val::Ct(c)) => c,
                        Some(Val::E(Expr { kind: ExprKind::Const(c), ty })) if ty.is_int() => c,
                        Some(Val::Poison) => return Err(()),
                        _ => {
                            return self.reject(
                                "EnumDecl",
                                format!("`{}.{}` = `{}` is not a compile-time integer", name, v.name, v.value),
                            )
                        }
                    }
                };
                if neg { -c } else { c }
            };
            if !tag.fits(val) {
                return self.reject(
                    "EnumDecl",
                    format!("`{}.{}` = {} does not fit the tag type {}", name, v.name, val, tag.name()),
                );
            }
            if variants.iter().any(|w| w.0 == v.name) {
                return self.reject("EnumDecl", format!("`{}` has two variants `{}`", name, v.name));
            }
            if let Some(w) = variants.iter().find(|w| w.1 == val) {
                return self.reject(
                    "EnumDecl",
                    format!("`{}.{}` and `{}.{}` have the same tag {}", name, w.0, name, v.name, val),
                );
            }
            variants.push((v.name.clone(), val));
            next = val + 1;
        }
        Ok(EnumDef { name, tag, bits, variants })
    }

    /// `E.v` (also `E::v`, and `.v` where an `E` is expected).
    fn enum_value(&mut self, id: u32, variant: &str) -> R<Val> {
        let def = &self.enums[id as usize];
        let tag = def.tag;
        match def.value(variant) {
            Some(c) => Ok(Val::P(Expr { ty: tag, kind: ExprKind::Const(c) }, LTy::Enum(id, tag))),
            None => {
                let e = def.name.clone();
                self.reject("ExprFieldAccess(enum)", format!("`{}` has no variant `{}`", e, variant))
            }
        }
    }

    /// `n` as a value of enum type `want`, when it is one of the forms that
    /// take their enum type from where they are used: `.v` and
    /// `@enumFromInt(x)`. None for any other expression.
    fn enum_literal(&mut self, n: &Node, want: &LTy) -> R<Option<Val>> {
        let LTy::Enum(id, tag) = *want else { return Ok(None) };
        if n.kind == NodeKind::ExprEnumValue {
            self.see(n);
            return self.enum_value(id, &n.name).map(Some);
        }
        if n.kind == NodeKind::ExprCall && n.name == "@enumFromInt" {
            self.see(n);
            if n.children.len() != 1 {
                return self.reject("ExprCall(@enumFromInt)", format!("{} arguments", n.children.len()));
            }
            let v = self.expr(&n.children[0])?;
            return self.enum_from_int(v, id, tag).map(Some);
        }
        Ok(None)
    }

    /// `@enumFromInt(v)` into enum `id`: a constant must be one of its tags
    /// (Zig refuses any other at compile time); a runtime value is checked
    /// and traps as `invalid enum value` when it is none of them.
    fn enum_from_int(&mut self, v: Val, id: u32, tag: Ty) -> R<Val> {
        let ename = self.enums[id as usize].name.clone();
        let lty = LTy::Enum(id, tag);
        let e = match v {
            Val::Poison => return Err(()),
            Val::Ct(c) => Expr { ty: tag, kind: ExprKind::Const(c) },
            Val::E(e) if e.ty.is_int() => e,
            v => {
                let d = self.val_desc(&v);
                return self.reject("ExprCall(@enumFromInt)", format!("{} into `{}`", d, ename));
            }
        };
        if let ExprKind::Const(c) = e.kind {
            if !self.enums[id as usize].variants.iter().any(|w| w.1 == c) {
                return self.reject("ExprCall(@enumFromInt)", format!("{} is no tag of `{}`", c, ename));
            }
            return Ok(Val::P(Expr { ty: tag, kind: ExprKind::Const(c) }, lty));
        }
        let mut tags: Vec<i128> = self.enums[id as usize].variants.iter().map(|w| w.1).collect();
        tags.sort();
        let (lo, hi) = (tags[0], *tags.last().unwrap());
        let n = tags.len() as i128;
        if hi - lo + 1 != n {
            return self.reject(
                "ExprCall(@enumFromInt)",
                format!("runtime value into `{}`, whose tags are not contiguous", ename),
            );
        }
        // idx = (x - lo) mod 2^64, which is below n exactly when x is a tag.
        let idx = if e.ty == Ty::U64 {
            if lo < 0 {
                return self.reject(
                    "ExprCall(@enumFromInt)",
                    format!("u64 value into `{}`, which has negative tags", ename),
                );
            }
            Expr {
                ty: Ty::U64,
                kind: ExprKind::Arith {
                    op: ArithOp::SubW,
                    lhs: Box::new(e),
                    rhs: Box::new(Expr { ty: Ty::U64, kind: ExprKind::Const(lo) }),
                    site: 0,
                },
            }
        } else {
            if lo - i64::MIN as i128 > (1i128 << 64) - n {
                return self.reject(
                    "ExprCall(@enumFromInt)",
                    format!("runtime value into `{}`, whose tags are too large", ename),
                );
            }
            let x = if e.ty == Ty::I64 { e } else { Expr { ty: Ty::I64, kind: ExprKind::Widen(Box::new(e)) } };
            let d = Expr {
                ty: Ty::I64,
                kind: ExprKind::Arith {
                    op: ArithOp::SubW,
                    lhs: Box::new(x),
                    rhs: Box::new(Expr { ty: Ty::I64, kind: ExprKind::Const(lo) }),
                    site: 0,
                },
            };
            Expr { ty: Ty::U64, kind: ExprKind::Cast { arg: Box::new(d), site: 0 } }
        };
        let site = self.site(TrapKind::EnumTag, format!("@enumFromInt into {}", ename), Ty::U64);
        let checked = Expr {
            ty: Ty::U64,
            kind: ExprKind::Bounds {
                idx: Box::new(idx),
                len: Box::new(Expr { ty: Ty::U64, kind: ExprKind::Const(n) }),
                site,
            },
        };
        let val = if lo == 0 {
            checked
        } else {
            Expr {
                ty: Ty::U64,
                kind: ExprKind::Arith {
                    op: ArithOp::AddW,
                    lhs: Box::new(checked),
                    rhs: Box::new(Expr { ty: Ty::U64, kind: ExprKind::Const(Ty::U64.wrap(lo)) }),
                    site: 0,
                },
            }
        };
        let val = if tag == Ty::U64 { val } else { Expr { ty: tag, kind: ExprKind::Cast { arg: Box::new(val), site: 0 } } };
        Ok(Val::P(val, lty))
    }

    /// `@intFromEnum(x)`: the tag. A constant one of a tag type t27b has no
    /// type for is a compile-time integer; a runtime one is allowed only
    /// where `use` says its storage type gives Zig's answer.
    fn int_from_enum(&mut self, n: &Node, use_: TagUse) -> R<Val> {
        self.see(n);
        if n.children.len() != 1 {
            return self.reject("ExprCall(@intFromEnum)", format!("{} arguments", n.children.len()));
        }
        if n.children[0].kind == NodeKind::ExprEnumValue {
            return self.reject(
                "ExprCall(@intFromEnum)",
                format!("of `.{}`, which has no enum type here", n.children[0].name),
            );
        }
        let (e, id) = match self.expr(&n.children[0])? {
            Val::P(e, LTy::Enum(id, _)) => (e, id),
            Val::Poison => return Err(()),
            v => {
                let d = self.val_desc(&v);
                return self.reject("ExprCall(@intFromEnum)", format!("of {}, not an enum", d));
            }
        };
        let def = &self.enums[id as usize];
        let (exact, bits, ename) = (def.exact(), def.bits, def.name.clone());
        if exact {
            return Ok(Val::E(e));
        }
        if let ExprKind::Const(c) = e.kind {
            return Ok(Val::Ct(c));
        }
        match use_ {
            TagUse::Compare => Ok(Val::E(e)),
            TagUse::Want(want) => {
                let holds = want.is_int() && if want.signed() { want.bits() > bits } else { want.bits() >= bits };
                if !holds {
                    return self.reject(
                        "type mismatch",
                        format!("expected {}, found u{} (the tag of `{}`)", want.name(), bits, ename),
                    );
                }
                if want == e.ty {
                    Ok(Val::E(e))
                } else if want.can_widen_from(e.ty) {
                    Ok(Val::E(Expr { ty: want, kind: ExprKind::Widen(Box::new(e)) }))
                } else {
                    // The value is below 2^bits, so it fits `want`.
                    Ok(Val::E(Expr { ty: want, kind: ExprKind::Cast { arg: Box::new(e), site: 0 } }))
                }
            }
            TagUse::Value => self.reject(
                "ExprCall(@intFromEnum auto-tag)",
                format!(
                    "the tag type of `{}` is u{}, which t27b has no type for; compare it or give it an integer result type",
                    ename, bits
                ),
            ),
        }
    }

    /// Two operands of a comparison: `.v` takes the enum type of the other
    /// side, and `@intFromEnum` of any width may be compared.
    fn operands(&mut self, x: &Node, y: &Node) -> R<(Val, Val)> {
        let lit = |n: &Node| n.kind == NodeKind::ExprEnumValue;
        let tag = |n: &Node| n.kind == NodeKind::ExprCall && n.name == "@intFromEnum";
        let one = |l: &mut Self, n: &Node| -> R<Val> {
            if tag(n) {
                match l.int_from_enum(n, TagUse::Compare) {
                    Err(()) if l.recover => Ok(Val::Poison),
                    r => r,
                }
            } else {
                l.expr(n)
            }
        };
        if lit(x) && !lit(y) {
            let b = one(self, y)?;
            let a = self.enum_operand(x, &b)?;
            return Ok((a, b));
        }
        if lit(y) && !lit(x) {
            let a = one(self, x)?;
            let b = self.enum_operand(y, &a)?;
            return Ok((a, b));
        }
        let a = one(self, x)?;
        let b = one(self, y)?;
        Ok((a, b))
    }

    /// `.v` compared with `other`, which must be an enum.
    fn enum_operand(&mut self, n: &Node, other: &Val) -> R<Val> {
        match other {
            Val::P(_, t @ LTy::Enum(..)) => {
                let t = t.clone();
                match self.enum_literal(n, &t) {
                    Err(()) if self.recover => Ok(Val::Poison),
                    r => r.map(|v| v.unwrap()),
                }
            }
            Val::Poison => Ok(Val::Poison),
            v => {
                let d = self.val_desc(v);
                self.see(n);
                self.reject("type mismatch", format!("`.{}` compared with {}, not an enum", n.name, d))
            }
        }
    }

    /// A comparison with an enum operand, or None when neither is one.
    /// `ordered`: one side is `E.v` or `E::v`, for which t27c's Zig backend
    /// writes `@intFromEnum(a) < @intFromEnum(b)`; on two enum values Zig has
    /// no `<`.
    fn enum_compare(&mut self, op: &str, a: &Val, b: &Val, ordered: bool) -> R<Option<Val>> {
        let (ea, ia) = match a {
            Val::P(e, LTy::Enum(id, _)) => (Some(e), Some(*id)),
            _ => (None, None),
        };
        let (eb, ib) = match b {
            Val::P(e, LTy::Enum(id, _)) => (Some(e), Some(*id)),
            _ => (None, None),
        };
        if ia.is_none() && ib.is_none() {
            return Ok(None);
        }
        if a.is_poison() || b.is_poison() {
            return Err(());
        }
        let cmp = match op {
            "==" => CmpOp::Eq,
            "!=" => CmpOp::Ne,
            "<" => CmpOp::Lt,
            "<=" => CmpOp::Le,
            ">" => CmpOp::Gt,
            ">=" => CmpOp::Ge,
            _ => return self.reject(&format!("ExprBinary({}) on enum", op), "arithmetic on an enum".into()),
        };
        if ia != ib || ia.is_none() || ib.is_none() {
            let (x, y) = (self.val_desc(a), self.val_desc(b));
            return self.reject("type mismatch", format!("`{}` on {} and {}", op, x, y));
        }
        if !matches!(cmp, CmpOp::Eq | CmpOp::Ne) && !ordered {
            let t = self.enums[ia.unwrap() as usize].name.clone();
            return self.reject(
                &format!("ExprBinary({}) on enum", op),
                format!("`{}` on two `{}` values, which Zig does not order", op, t),
            );
        }
        let (x, y) = (ea.unwrap().clone(), eb.unwrap().clone());
        if let (ExprKind::Const(p), ExprKind::Const(q)) = (&x.kind, &y.kind) {
            return Ok(Some(Val::E(Expr { ty: Ty::Bool, kind: ExprKind::Const(cmp.holds(*p, *q) as i128) })));
        }
        Ok(Some(Val::E(Expr { ty: Ty::Bool, kind: ExprKind::Cmp { op: cmp, lhs: Box::new(x), rhs: Box::new(y) } })))
    }

    /// `E.v` or `E::v` of a declared enum (what t27c's Zig backend orders by
    /// tag).
    fn names_variant(&self, n: &Node) -> bool {
        match n.kind {
            NodeKind::ExprFieldAccess => n.children.first().is_some_and(|b| {
                b.kind == NodeKind::ExprIdentifier && self.enum_nodes.contains_key(&b.name)
            }),
            NodeKind::ExprIdentifier => {
                n.name.split_once("::").is_some_and(|(e, _)| self.enum_nodes.contains_key(e))
            }
            _ => false,
        }
    }

    /// Lay out struct `id` (C rules) if that has not been done.
    fn layout(&mut self, id: u32) -> R<()> {
        let sd = &self.structs[id as usize];
        if sd.size.is_some() {
            return Ok(());
        }
        if let Some(r) = sd.fail.clone() {
            // Reported where it was first used; every later use says so too,
            // except in recovery mode, where a construct is named once.
            if !self.recover {
                self.errors.push(Reject { line: self.line, ..r });
            }
            return Err(());
        }
        let name = sd.name.clone();
        let key = format!("struct {}", name);
        if !self.resolving.insert(key.clone()) {
            return self.reject("StructDecl", format!("`{}` contains itself", name));
        }
        let node = self.struct_nodes[&name];
        let saved = self.line;
        let src = self.src;
        if let Some(l) = src.and_then(|s| header_line(s, "const", &name).or_else(|| header_line(s, "pub const", &name))) {
            self.line = l;
        }
        let nerr = self.errors.len();
        let r = self.layout_fields(node);
        self.line = saved;
        self.resolving.remove(&key);
        match r {
            Ok((fields, size, align)) => {
                let sd = &mut self.structs[id as usize];
                sd.fields = fields;
                sd.size = Some(size);
                sd.align = align;
                Ok(())
            }
            Err(()) => {
                if let Some(e) = self.errors.get(nerr).cloned() {
                    self.structs[id as usize].fail = Some(e);
                }
                Err(())
            }
        }
    }

    fn layout_fields(&mut self, node: &'a Node) -> R<(Vec<Field<'a>>, u32, u32)> {
        if !node.params.is_empty() {
            return self.reject("StructDecl", format!("generic struct `{}`", node.name));
        }
        let mut fields: Vec<Field<'a>> = Vec::new();
        let (mut size, mut align) = (0u32, 1u32);
        for f in &node.children {
            if f.kind != NodeKind::ExprIdentifier || f.extra_type.trim().is_empty() || f.children.len() > 1 {
                let k = kind_name(f);
                return self.reject("StructDecl", format!("`{}` member of kind {}", node.name, k));
            }
            if fields.iter().any(|g| g.name == f.name) {
                return self.reject("StructDecl", format!("`{}` has two fields `{}`", node.name, f.name));
            }
            let ty = self.lty(&f.extra_type)?;
            let (fs, fa) = self.size_align(&ty)?;
            let off = size.div_ceil(fa) * fa;
            size = off + fs;
            align = align.max(fa);
            fields.push(Field { name: f.name.clone(), ty, off, default: f.children.first() });
        }
        Ok((fields, size.div_ceil(align) * align, align))
    }

    fn size_align(&mut self, t: &LTy) -> R<(u32, u32)> {
        match t {
            LTy::S(ty) => Ok((ty.bytes(), ty.bytes())),
            LTy::Ptr(..) => Ok((8, 8)),
            LTy::Enum(_, ty) => Ok((ty.bytes(), ty.bytes())),
            LTy::Str | LTy::Slice(..) => Ok((16, 8)),
            LTy::Struct(id) => {
                self.layout(*id)?;
                let sd = &self.structs[*id as usize];
                Ok((sd.size.unwrap(), sd.align))
            }
            LTy::Arr(inner, n) => {
                let (es, ea) = self.size_align(inner)?;
                match es.checked_mul(*n) {
                    Some(size) if size < 1 << 30 => Ok((size, ea)),
                    _ => {
                        let what = self.type_name(t);
                        self.reject("type [N]T", format!("`{}` is too large", what))
                    }
                }
            }
        }
    }

    fn type_name(&self, t: &LTy) -> String {
        match t {
            LTy::S(ty) => ty.name().to_string(),
            LTy::Ptr(inner, m) => format!("*{}{}", if *m { "" } else { "const " }, self.type_name(inner)),
            LTy::Struct(id) => self.structs[*id as usize].name.clone(),
            LTy::Enum(id, _) => self.enums[*id as usize].name.clone(),
            LTy::Str => "str".to_string(),
            LTy::Arr(inner, n) => format!("[{}]{}", n, self.type_name(inner)),
            LTy::Slice(inner, m) => format!("[]{}{}", if *m { "" } else { "const " }, self.type_name(inner)),
        }
    }

    /// A new frame slot for one value of type `t`.
    fn new_slot(&mut self, t: &LTy) -> R<u32> {
        let (size, align) = self.size_align(t)?;
        self.slots.push(SlotInfo { size: size.max(1), align: align.max(1) });
        Ok((self.slots.len() - 1) as u32)
    }

    fn fields(&mut self, id: u32) -> R<Vec<Field<'a>>> {
        self.layout(id)?;
        Ok(self.structs[id as usize].fields.clone())
    }

    /// Evaluate `n` as a value of type `want`: a struct literal takes its
    /// type from here, so it may be anonymous (`.{ ... }`).
    fn expr_as(&mut self, n: &Node, want: &LTy) -> R<Val> {
        if let Some(v) = self.enum_literal(n, want)? {
            return Ok(v);
        }
        if let (NodeKind::ExprCall, "@intFromEnum", LTy::S(ty)) = (&n.kind, n.name.as_str(), want) {
            let v = self.int_from_enum(n, TagUse::Want(*ty))?;
            return self.coerce_to(v, want);
        }
        if n.kind == NodeKind::ExprStructLit && matches!(want, LTy::Struct(_)) {
            self.see(n);
            self.lit_type(n, want)?;
            return self.struct_temp(n, want.clone());
        }
        if n.kind == NodeKind::ExprArrayLiteral && matches!(want, LTy::Arr(..)) {
            self.see(n);
            return self.struct_temp(n, want.clone());
        }
        // `&[_]T{ ... }` where a `[]const T` is wanted: the literal in a
        // temporary, and a slice of all of it.
        if n.kind == NodeKind::ExprUnary
            && n.extra_op == "&"
            && n.children.len() == 1
            && n.children[0].kind == NodeKind::ExprArrayLiteral
        {
            let elem = match want {
                LTy::Str => Some(LTy::S(Ty::U8)),
                LTy::Slice(t, false) => Some((**t).clone()),
                _ => None,
            };
            if let Some(elem) = elem {
                self.see(n);
                self.see(&n.children[0]);
                let len = n.children[0].children.len() as u32;
                let Val::M(arr) = self.struct_temp(&n.children[0], LTy::Arr(Box::new(elem), len))? else {
                    return Err(());
                };
                return self.slice_of(addr_of(&arr), len, want.clone());
            }
        }
        let v = self.expr(n)?;
        self.coerce_to(v, want)
    }

    /// A slice of type `t` (a `Slice` or `Str`) of all `len` elements at
    /// `ptr`, as a fresh temporary.
    fn slice_of(&mut self, ptr: Expr, len: u32, t: LTy) -> R<Val> {
        let k = self.new_slot(&t)?;
        let n = Expr { ty: Ty::U64, kind: ExprKind::Const(len as i128) };
        let stmts = vec![
            Stmt::Store { addr: slot_expr(k), off: 0, value: ptr },
            Stmt::Store { addr: slot_expr(k), off: 8, value: n },
        ];
        let addr = Expr { ty: Ty::Ptr, kind: ExprKind::Seq { stmts, value: Box::new(slot_expr(k)) } };
        Ok(Val::M(Place { addr, off: 0, ty: t, mutable: false, temp: Some(k) }))
    }

    /// The header of slice place `p`, at a pure address: `p` itself, or a
    /// copy of it made by `pin` (which runs first, once).
    fn pin_header(&mut self, p: &Place, pin: &mut Vec<Stmt>) -> R<(Expr, u32)> {
        if pure_addr(&p.addr) {
            return Ok((p.addr.clone(), p.off));
        }
        let k = self.new_slot(&p.ty)?;
        pin.push(Stmt::Copy { dst: slot_expr(k), src: addr_of(p), size: 16 });
        Ok((slot_expr(k), 0))
    }

    /// Check a struct literal's own name, if it has one, against `want`.
    fn lit_type(&mut self, n: &Node, want: &LTy) -> R<()> {
        if n.name.is_empty() {
            return Ok(());
        }
        let t = self.lty(&n.name)?;
        if &t != want {
            let (a, b) = (self.type_name(want), self.type_name(&t));
            return self.reject("type mismatch", format!("expected {}, found {}", a, b));
        }
        Ok(())
    }

    fn coerce_to(&mut self, v: Val, want: &LTy) -> R<Val> {
        if v.is_poison() {
            // Recovery mode: the caller decides what an unknown value costs.
            return Ok(v);
        }
        match want {
            LTy::S(ty) => Ok(Val::E(self.coerce(v, *ty)?)),
            LTy::Enum(id, _) => match v {
                Val::P(e, LTy::Enum(got, t)) if got == *id => Ok(Val::P(e, LTy::Enum(got, t))),
                v => {
                    let (a, b) = (self.type_name(want), self.val_desc(&v));
                    self.reject("type mismatch", format!("expected {}, found {}", a, b))
                }
            },
            LTy::Ptr(inner, m) => match v {
                // `*T` coerces to `*const T`.
                Val::P(e, LTy::Ptr(got, gm)) if got == *inner && (gm || !*m) => Ok(Val::P(e, want.clone())),
                Val::P(_, t) => {
                    let (a, b) = (self.type_name(want), self.type_name(&t));
                    self.reject("type mismatch", format!("expected {}, found {}", a, b))
                }
                _ => {
                    let a = self.type_name(want);
                    self.reject("type mismatch", format!("expected {}, found a value", a))
                }
            },
            // A literal is written into a temporary; the place it is copied
            // to, if any, is the caller's business.
            LTy::Str => match v {
                Val::S(k, len) => {
                    let slot = self.new_slot(want)?;
                    let dst = Place { addr: slot_expr(slot), off: 0, ty: LTy::Str, mutable: true, temp: None };
                    let mut stmts = Vec::new();
                    self.store_str(&dst, k, len, &mut stmts);
                    let addr = Expr { ty: Ty::Ptr, kind: ExprKind::Seq { stmts, value: Box::new(slot_expr(slot)) } };
                    Ok(Val::M(Place { addr, off: 0, ty: LTy::Str, mutable: false, temp: Some(slot) }))
                }
                Val::M(p) if p.ty == LTy::Str => Ok(Val::M(p)),
                // `[]u8` coerces to `[]const u8`.
                Val::M(mut p) if p.ty == LTy::Slice(Box::new(LTy::S(Ty::U8)), true) => {
                    p.ty = LTy::Str;
                    Ok(Val::M(p))
                }
                Val::M(p) => {
                    let b = self.type_name(&p.ty);
                    self.reject("type mismatch", format!("expected str, found {}", b))
                }
                // `*[N]u8` (or `*const [N]u8`) coerces to `[]const u8`.
                Val::P(e, LTy::Ptr(inner, _)) if matches!(&*inner, LTy::Arr(t, _) if **t == LTy::S(Ty::U8)) => {
                    let LTy::Arr(_, n) = *inner else { unreachable!() };
                    self.slice_of(e, n, LTy::Str)
                }
                Val::P(_, t) => {
                    let b = self.type_name(&t);
                    self.reject("type mismatch", format!("expected str, found {}", b))
                }
                _ => self.reject("type mismatch", "expected str, found a scalar".into()),
            },
            LTy::Slice(inner, m) => match v {
                // `[]T` coerces to `[]const T`.
                Val::M(mut p) if matches!(&p.ty, LTy::Slice(t, pm) if t == inner && (*pm || !*m)) => {
                    p.ty = want.clone();
                    Ok(Val::M(p))
                }
                // `*[N]T` coerces to `[]T`, `*const [N]T` only to `[]const T`.
                Val::P(e, LTy::Ptr(pt, pm))
                    if (pm || !*m) && matches!(&*pt, LTy::Arr(t, _) if t == inner) =>
                {
                    let LTy::Arr(_, n) = *pt else { unreachable!() };
                    self.slice_of(e, n, want.clone())
                }
                v => {
                    let (a, b) = (self.type_name(want), self.val_desc(&v));
                    self.reject("type mismatch", format!("expected {}, found {}", a, b))
                }
            },
            LTy::Arr(..) => match v {
                Val::M(p) if &p.ty == want => Ok(Val::M(p)),
                Val::A(t, elems) if &t == want => Ok(Val::M(self.materialize(t, elems)?)),
                v => {
                    let (a, b) = (self.type_name(want), self.val_desc(&v));
                    self.reject("type mismatch", format!("expected {}, found {}", a, b))
                }
            },
            LTy::Struct(_) => match v {
                Val::M(p) if &p.ty == want => Ok(Val::M(p)),
                Val::M(p) => {
                    let (a, b) = (self.type_name(want), self.type_name(&p.ty));
                    self.reject("type mismatch", format!("expected {}, found {}", a, b))
                }
                _ => {
                    let a = self.type_name(want);
                    self.reject("type mismatch", format!("expected {}, found a scalar", a))
                }
            },
        }
    }

    /// The register expression of a scalar or pointer value.
    fn reg(&mut self, v: Val) -> R<Expr> {
        match v {
            Val::E(e) | Val::P(e, _) => Ok(e),
            Val::Poison => Err(()),
            Val::Ct(_) => self.reject("type mismatch", "untyped integer literal".into()),
            Val::M(_) | Val::A(..) => self.reject("type mismatch", "a struct or array where a scalar is expected".into()),
            Val::S(..) => self.reject("type mismatch", "a string where a scalar is expected".into()),
        }
    }

    /// The blob of a string literal's bytes (deduplicated), as a value.
    fn string(&mut self, bytes: &[u8]) -> Val {
        let len = bytes.len() as u64;
        if let Some(&k) = self.strings.get(bytes) {
            return Val::S(k, len);
        }
        // A trailing NUL, as Zig's literals have, so no blob is empty.
        let mut blob = bytes.to_vec();
        blob.push(0);
        self.data.push(blob);
        let k = (self.data.len() - 1) as u32;
        self.strings.insert(bytes.to_vec(), k);
        Val::S(k, len)
    }

    /// `dst = literal`: the address of the bytes, then the length. `dst`'s
    /// address is pure.
    fn store_str(&mut self, dst: &Place, k: u32, len: u64, out: &mut Vec<Stmt>) {
        let ptr = Expr { ty: Ty::Ptr, kind: ExprKind::Data(k) };
        out.push(Stmt::Store { addr: dst.addr.clone(), off: dst.off, value: ptr });
        let n = Expr { ty: Ty::U64, kind: ExprKind::Const(len as i128) };
        out.push(Stmt::Store { addr: dst.addr.clone(), off: dst.off + 8, value: n });
    }

    /// A compile-time array, written into a fresh temporary at the point
    /// the value is evaluated.
    fn materialize(&mut self, t: LTy, elems: Vec<Val>) -> R<Place> {
        let k = self.new_slot(&t)?;
        let dst = Place { addr: slot_expr(k), off: 0, ty: t.clone(), mutable: true, temp: None };
        let mut stmts = Vec::new();
        self.store_const(&dst, &elems, &mut stmts)?;
        let addr = if stmts.is_empty() {
            slot_expr(k)
        } else {
            Expr { ty: Ty::Ptr, kind: ExprKind::Seq { stmts, value: Box::new(slot_expr(k)) } }
        };
        Ok(Place { addr, off: 0, ty: t, mutable: false, temp: Some(k) })
    }

    /// The elements of a compile-time array into `dst` (pure address).
    fn store_const(&mut self, dst: &Place, elems: &[Val], out: &mut Vec<Stmt>) -> R<()> {
        let LTy::Arr(elem, _) = &dst.ty else { unreachable!() };
        let elem = (**elem).clone();
        let (esize, _) = self.size_align(&elem)?;
        for (i, v) in elems.iter().enumerate() {
            let p = elem_place(dst, &elem, esize, i as u32);
            match v {
                Val::S(k, len) => self.store_str(&p, *k, *len, out),
                Val::A(_, sub) => self.store_const(&p, sub, out)?,
                _ => return self.reject("ConstDecl", "internal: array constant element".into()),
            }
        }
        Ok(())
    }

    /// What a value is, for a type error.
    fn val_desc(&self, v: &Val) -> String {
        match v {
            Val::Ct(_) | Val::E(_) => "a scalar".into(),
            Val::P(_, t @ LTy::Enum(..)) => self.type_name(t),
            Val::P(..) => "a pointer".into(),
            Val::S(..) => "a string".into(),
            Val::M(p) => self.type_name(&p.ty),
            Val::A(t, _) => self.type_name(t),
            Val::Poison => "an unknown value".into(),
        }
    }

    fn is_str(&self, v: &Val) -> bool {
        match v {
            Val::S(..) => true,
            Val::M(p) => p.ty == LTy::Str,
            _ => false,
        }
    }

    /// `a == b` (or `!=`) where one side is a string: content equality.
    fn str_eq(&mut self, negate: bool, a: Val, b: Val) -> R<Val> {
        let eq = if let (Val::S(ka, la), Val::S(kb, lb)) = (&a, &b) {
            let (x, y) = (&self.data[*ka as usize][..*la as usize], &self.data[*kb as usize][..*lb as usize]);
            Expr { ty: Ty::Bool, kind: ExprKind::Const((x == y) as i128) }
        } else {
            let mut args = Vec::new();
            for v in [a, b] {
                match self.coerce_to(v, &LTy::Str)? {
                    Val::M(p) => args.push(addr_of(&p)),
                    _ => return Err(()),
                }
            }
            self.eql_used = true;
            Expr { ty: Ty::Bool, kind: ExprKind::Call { func: self.nfuncs, args } }
        };
        if !negate {
            return Ok(Val::E(eq));
        }
        Ok(Val::E(match eq.kind {
            ExprKind::Const(c) => Expr { ty: Ty::Bool, kind: ExprKind::Const(1 - c) },
            _ => Expr { ty: Ty::Bool, kind: ExprKind::Not(Box::new(eq)) },
        }))
    }

    /// A struct or array literal built in a fresh temporary slot, at the
    /// point the value is evaluated.
    fn struct_temp(&mut self, n: &Node, t: LTy) -> R<Val> {
        let k = self.new_slot(&t)?;
        let dst = Place { addr: slot_expr(k), off: 0, ty: t, mutable: true, temp: None };
        let mut stmts = Vec::new();
        self.init(n, dst.clone(), true, &mut stmts)?;
        let addr = if stmts.is_empty() {
            slot_expr(k)
        } else {
            Expr { ty: Ty::Ptr, kind: ExprKind::Seq { stmts, value: Box::new(slot_expr(k)) } }
        };
        Ok(Val::M(Place { addr, off: 0, ty: dst.ty, mutable: false, temp: Some(k) }))
    }

    /// Write the value of `n` into `dst`. `fresh`: nothing can read `dst`
    /// while it is being written (a new variable, a temporary, a result), so
    /// a struct literal or a struct-returning call may build in place.
    fn init(&mut self, n: &Node, dst: Place, fresh: bool, out: &mut Vec<Stmt>) -> R<()> {
        self.see(n);
        if is_undefined(n) {
            return Ok(());
        }
        let t = dst.ty.clone();
        let in_place = n.kind == NodeKind::ExprCall
            && fresh
            && self.sigs.get(&n.name).is_some_and(|s| s.ret == Some(t.clone()));
        if matches!(t, LTy::Str | LTy::Slice(..)) && !in_place {
            let v = if t == LTy::Str && n.kind != NodeKind::ExprUnary { self.expr(n)? } else { self.expr_as(n, &t)? };
            return match v {
                Val::S(k, len) if pure_addr(&dst.addr) => {
                    self.store_str(&dst, k, len, out);
                    Ok(())
                }
                v => match self.coerce_to(v, &t)? {
                    Val::M(src) => self.copy(&dst, src, out),
                    _ => Err(()),
                },
            };
        }
        if let LTy::Arr(..) = t {
            if n.kind == NodeKind::ExprArrayLiteral {
                if fresh && pure_addr(&dst.addr) {
                    return self.init_array(n, &dst, out);
                }
                // The literal may read `dst`: build it aside first.
                let k = self.new_slot(&t)?;
                let tmp = Place { addr: slot_expr(k), off: 0, ty: t, mutable: true, temp: None };
                self.init_array(n, &tmp, out)?;
                return self.copy(&dst, tmp, out);
            }
            if in_place {
                let (call, _, _) = self.call(n, Some(addr_of(&dst)))?;
                out.push(Stmt::Eval(call));
                return Ok(());
            }
            let v = self.expr(n)?;
            return match v {
                Val::A(u, elems) if u == t && pure_addr(&dst.addr) => self.store_const(&dst, &elems, out),
                v => match self.coerce_to(v, &t)? {
                    Val::M(src) => self.copy(&dst, src, out),
                    _ => Err(()),
                },
            };
        }
        let LTy::Struct(id) = t else {
            if in_place {
                // A call that returns a str, built in place.
                let (call, _, _) = self.call(n, Some(addr_of(&dst)))?;
                out.push(Stmt::Eval(call));
                return Ok(());
            }
            let v = self.expr_as(n, &t)?;
            let value = self.reg(v)?;
            out.push(Stmt::Store { addr: dst.addr, off: dst.off, value });
            return Ok(());
        };
        if n.kind == NodeKind::ExprStructLit {
            self.lit_type(n, &t)?;
            if fresh && pure_addr(&dst.addr) {
                return self.init_struct(n, id, &dst, out);
            }
            let k = self.new_slot(&t)?;
            let tmp = Place { addr: slot_expr(k), off: 0, ty: t, mutable: true, temp: None };
            self.init_struct(n, id, &tmp, out)?;
            return self.copy(&dst, tmp, out);
        }
        if n.kind == NodeKind::ExprCall && fresh && self.sigs.get(&n.name).is_some_and(|s| s.ret == Some(t.clone())) {
            let (call, _, _) = self.call(n, Some(addr_of(&dst)))?;
            out.push(Stmt::Eval(call));
            return Ok(());
        }
        let v = self.expr_as(n, &t)?;
        match v {
            Val::M(src) => self.copy(&dst, src, out),
            Val::Poison => Err(()),
            _ => self.reject("type mismatch", "internal: struct value not in memory".into()),
        }
    }

    /// The elements of an array literal, into `dst` (whose address is pure).
    /// The literal's own type, if it names one (`[_]u8{ ... }`), is not
    /// checked: t27c's Zig backend writes every array literal as an
    /// anonymous `.{ ... }`, which takes the type of its destination.
    fn init_array(&mut self, n: &Node, dst: &Place, out: &mut Vec<Stmt>) -> R<()> {
        self.array_count(n, &dst.ty)?;
        let LTy::Arr(elem, _) = &dst.ty else { unreachable!() };
        let elem = (**elem).clone();
        let (esize, _) = self.size_align(&elem)?;
        for (i, c) in n.children.iter().enumerate() {
            let p = elem_place(dst, &elem, esize, i as u32);
            self.init(c, p, true, out)?;
        }
        Ok(())
    }

    /// The fields of a struct literal, into `dst` (whose address is pure).
    fn init_struct(&mut self, n: &Node, id: u32, dst: &Place, out: &mut Vec<Stmt>) -> R<()> {
        let fields = self.fields(id)?;
        let sname = self.structs[id as usize].name.clone();
        let mut seen = vec![false; fields.len()];
        for c in &n.children {
            self.see(c);
            if c.kind != NodeKind::ExprFieldAccess || c.children.len() != 1 {
                return self.reject("ExprStructLit", format!("positional initializer in a `{}` literal", sname));
            }
            let Some(i) = fields.iter().position(|f| f.name == c.name) else {
                return self.reject("ExprStructLit", format!("`{}` has no field `{}`", sname, c.name));
            };
            if seen[i] {
                return self.reject("ExprStructLit", format!("field `{}` initialised twice", c.name));
            }
            seen[i] = true;
            let sub = field_place(dst, &fields[i]);
            self.init(&c.children[0], sub, true, out)?;
        }
        for (i, f) in fields.iter().enumerate() {
            if seen[i] {
                continue;
            }
            let Some(d) = f.default else {
                return self.reject("ExprStructLit", format!("missing field `{}` of `{}`", f.name, sname));
            };
            // A default sees module scope only.
            let saved = std::mem::replace(&mut self.scopes, vec![HashMap::new()]);
            let r = self.init(d, field_place(dst, f), true, out);
            self.scopes = saved;
            r?;
        }
        Ok(())
    }

    /// `dst = src` for a struct: one copy. Both addresses are evaluated, dst
    /// first, even when there is nothing to copy.
    fn copy(&mut self, dst: &Place, src: Place, out: &mut Vec<Stmt>) -> R<()> {
        let (size, _) = self.size_align(&dst.ty)?;
        if size == 0 {
            for p in [dst, &src] {
                if !pure_addr(&p.addr) {
                    out.push(Stmt::Eval(p.addr.clone()));
                }
            }
            return Ok(());
        }
        out.push(Stmt::Copy { dst: addr_of(dst), src: addr_of(&src), size });
        Ok(())
    }

    /// The value stored at a place: a load for a scalar or pointer (folded
    /// when the place is read-only data), the place itself for a struct.
    fn place_value(&mut self, p: Place) -> R<Val> {
        let Some(ty) = reg_ty(&p.ty) else { return Ok(Val::M(p)) };
        if let (ExprKind::Data(k), LTy::S(_) | LTy::Enum(..)) = (&p.addr.kind, &p.ty) {
            let blob = &self.data[*k as usize];
            let mut raw = 0u64;
            for i in (0..ty.bytes() as usize).rev() {
                raw = (raw << 8) | blob[p.off as usize + i] as u64;
            }
            return Ok(val_of(Expr { ty, kind: ExprKind::Const(ty.from_raw(raw)) }, &p.ty));
        }
        let e = Expr { ty, kind: ExprKind::Load { addr: Box::new(p.addr), off: p.off } };
        Ok(val_of(e, &p.ty))
    }

    /// The memory an expression names: a variable in memory, a field, or
    /// `p.*`.
    fn lvalue(&mut self, n: &Node) -> R<Place> {
        self.see(n);
        match n.kind {
            NodeKind::ExprIdentifier => match self.lookup(&n.name) {
                Some(Binding::Mem(p)) => Ok(p),
                Some(Binding::Const(Val::Poison)) => Err(()),
                Some(Binding::Const(Val::A(t, elems))) => self.materialize(t, elems),
                Some(_) => self.reject("ExprUnary(&)", format!("`{}` is not in memory", n.name)),
                None => match self.global(&n.name)? {
                    Some(Val::M(p)) => Ok(p),
                    Some(Val::A(t, elems)) => self.materialize(t, elems),
                    Some(Val::Poison) => Err(()),
                    Some(_) => self.reject("ExprUnary(&)", format!("address of constant `{}`", n.name)),
                    None => self.unknown_name(&n.name),
                },
            },
            NodeKind::ExprFieldAccess if n.children.len() == 1 => match self.member(n)? {
                Ok(p) => Ok(p),
                Err(_) => self.reject("ExprFieldAccess(.len)", "`.len` of an array is not a place".into()),
            },
            NodeKind::ExprIndex => match self.index(n)? {
                Ok(p) => Ok(p),
                Err(_) => self.reject("StmtAssign", "assignment through a constant".into()),
            },
            _ => {
                let k = kind_name(n);
                self.reject(&k, "not addressable".into())
            }
        }
    }

    /// `base[i]`: the place of an array element, or, for a constant index
    /// into a compile-time array, the element itself. A constant index out
    /// of range is rejected, as Zig rejects it at compile time; any other
    /// index is checked when it is evaluated and traps out of range, as
    /// Zig's safety check does.
    fn index(&mut self, n: &Node) -> R<Result<Place, Val>> {
        // `x[a..b]` parses as an index whose index is the range `a..b`;
        // `x[a:b]` and `x[a..]` as their own slice node.
        if n.extra_op.is_empty()
            && n.children.len() == 2
            && n.children[1].kind == NodeKind::ExprBinary
            && n.children[1].extra_op == ".."
            && n.children[1].children.len() == 2
        {
            self.see(&n.children[1]);
            let r = &n.children[1];
            return self.slicing(&n.children[0], &r.children[0], Some(&r.children[1])).map(Err);
        }
        match (n.extra_op.as_str(), n.children.len()) {
            ("slice", 3) => return self.slicing(&n.children[0], &n.children[1], Some(&n.children[2])).map(Err),
            ("slice_open", 2) => return self.slicing(&n.children[0], &n.children[1], None).map(Err),
            ("", 2) => {}
            ("", _) => return self.reject("ExprIndex", "unexpected shape".into()),
            (op, _) => return self.reject(&format!("ExprIndex({})", op), "unexpected shape".into()),
        }
        let base = self.expr(&n.children[0])?;
        let idx = self.expr(&n.children[1])?;
        if base.is_poison() || idx.is_poison() {
            return Err(());
        }
        // A string literal at a constant index is a constant.
        if let Val::S(k, len) = base {
            let c = match &idx {
                Val::Ct(c) => Some(*c),
                Val::E(Expr { kind: ExprKind::Const(c), ty }) if ty.is_int() && !ty.signed() => Some(*c),
                _ => None,
            };
            if let Some(c) = c {
                if !(0..len as i128).contains(&c) {
                    return self.reject("ExprIndex", format!("index {} out of bounds for a string of length {}", c, len));
                }
                let b = self.data[k as usize][c as usize];
                return Ok(Err(Val::E(Expr { ty: Ty::U8, kind: ExprKind::Const(b as i128) })));
            }
        }
        let base = match base {
            v @ Val::S(..) => self.coerce_to(v, &LTy::Str)?,
            v => v,
        };
        if let Val::M(p) = &base {
            if matches!(p.ty, LTy::Str | LTy::Slice(..)) {
                let Val::M(p) = base else { unreachable!() };
                return self.slice_index(p, idx).map(Ok);
            }
        }
        let p = match base {
            Val::A(t, elems) => {
                let c = match &idx {
                    Val::Ct(c) => Some(*c),
                    Val::E(Expr { kind: ExprKind::Const(c), ty }) if ty.is_int() && !ty.signed() => Some(*c),
                    _ => None,
                };
                if let Some(c) = c {
                    if (0..elems.len() as i128).contains(&c) {
                        return Ok(Err(elems[c as usize].clone()));
                    }
                }
                self.materialize(t, elems)?
            }
            Val::M(p) if matches!(p.ty, LTy::Arr(..)) => p,
            Val::P(e, LTy::Ptr(inner, m)) if matches!(*inner, LTy::Arr(..)) => {
                Place { addr: e, off: 0, ty: *inner, mutable: m, temp: None }
            }
            v => {
                let d = self.val_desc(&v);
                return self.reject("ExprIndex", format!("index of {}", d));
            }
        };
        let LTy::Arr(elem, len) = p.ty.clone() else { unreachable!() };
        let (esize, _) = self.size_align(&elem)?;
        let mutable = p.mutable && p.temp.is_none();
        // Zig: the index is a usize.
        let e = self.coerce(idx, Ty::U64)?;
        if let ExprKind::Const(c) = e.kind {
            if c >= len as i128 {
                let t = self.type_name(&p.ty);
                return self.reject("ExprIndex", format!("index {} out of bounds for `{}`", c, t));
            }
            let mut q = elem_place(&p, &elem, esize, c as u32);
            q.mutable = mutable;
            return Ok(Ok(q));
        }
        let t = self.type_name(&p.ty);
        let site = self.site(TrapKind::Bounds, format!("index of {}", t), Ty::U64);
        let len = Expr { ty: Ty::U64, kind: ExprKind::Const(len as i128) };
        let checked = Expr { ty: Ty::U64, kind: ExprKind::Bounds { idx: Box::new(e), len: Box::new(len), site } };
        let addr = Expr {
            ty: Ty::Ptr,
            kind: ExprKind::Offset { base: Box::new(addr_of(&p)), idx: Box::new(checked), scale: esize },
        };
        Ok(Ok(Place { addr, off: 0, ty: *elem, mutable, temp: None }))
    }

    /// The element type of a slice type and whether its elements may be
    /// written.
    fn slice_elem(t: &LTy) -> (LTy, bool) {
        match t {
            LTy::Slice(elem, m) => ((**elem).clone(), *m),
            _ => (LTy::S(Ty::U8), false),
        }
    }

    /// `s[i]` for a slice or str `s`: the header is read once, the index is
    /// checked against its length when evaluated, and traps out of range.
    fn slice_index(&mut self, p: Place, idx: Val) -> R<Place> {
        let (elem, m) = Self::slice_elem(&p.ty);
        let (esize, _) = self.size_align(&elem)?;
        let mut pin = Vec::new();
        let (hdr, off) = self.pin_header(&p, &mut pin)?;
        let e = self.coerce(idx, Ty::U64)?;
        let t = self.type_name(&p.ty);
        let site = self.site(TrapKind::Bounds, format!("index of {}", t), Ty::U64);
        let mut ptr = Expr { ty: Ty::Ptr, kind: ExprKind::Load { addr: Box::new(hdr.clone()), off } };
        if !pin.is_empty() {
            ptr = Expr { ty: Ty::Ptr, kind: ExprKind::Seq { stmts: pin, value: Box::new(ptr) } };
        }
        let len = Expr { ty: Ty::U64, kind: ExprKind::Load { addr: Box::new(hdr), off: off + 8 } };
        let checked = Expr { ty: Ty::U64, kind: ExprKind::Bounds { idx: Box::new(e), len: Box::new(len), site } };
        let addr = Expr { ty: Ty::Ptr, kind: ExprKind::Offset { base: Box::new(ptr), idx: Box::new(checked), scale: esize } };
        Ok(Place { addr, off: 0, ty: elem, mutable: m, temp: None })
    }

    /// `x[i..j]` (or `x[i..]`, to the end) of an array, a pointer to an
    /// array, a slice or a string: a new slice, built in a temporary. As in
    /// Zig, `i <= j <= len` is checked when the slice is evaluated (at
    /// compile time where all three are constants) and traps otherwise.
    fn slicing(&mut self, base: &Node, start: &Node, end: Option<&Node>) -> R<Val> {
        let construct = if end.is_some() { "ExprIndex(slice)" } else { "ExprIndex(slice_open)" };
        let base = self.expr(base)?;
        if base.is_poison() {
            return Err(());
        }
        let base = match base {
            v @ Val::S(..) => self.coerce_to(v, &LTy::Str)?,
            Val::A(t, elems) => Val::M(self.materialize(t, elems)?),
            v => v,
        };
        // The source: an array place or a slice place.
        let (p, mutable) = match base {
            Val::M(p) if matches!(p.ty, LTy::Arr(..)) => {
                let m = p.mutable && p.temp.is_none();
                (p, m)
            }
            Val::P(e, LTy::Ptr(inner, m)) if matches!(*inner, LTy::Arr(..)) => {
                (Place { addr: e, off: 0, ty: *inner, mutable: m, temp: None }, m)
            }
            Val::M(p) if matches!(p.ty, LTy::Str | LTy::Slice(..)) => {
                let m = Self::slice_elem(&p.ty).1;
                (p, m)
            }
            v => {
                let d = self.val_desc(&v);
                return self.reject(construct, format!("slice of {}", d));
            }
        };
        let elem = match &p.ty {
            LTy::Arr(elem, _) => (**elem).clone(),
            t => Self::slice_elem(t).0,
        };
        let (esize, _) = self.size_align(&elem)?;
        let rt = if elem == LTy::S(Ty::U8) && !mutable { LTy::Str } else { LTy::Slice(Box::new(elem), mutable) };
        let tname = self.type_name(&p.ty);
        let hdr = self.new_slot(&rt)?;
        let scr = self.new_slot(&LTy::S(Ty::U64))?;
        let at = |k: u32, off: u32, ty: Ty| Expr { ty, kind: ExprKind::Load { addr: Box::new(slot_expr(k)), off } };
        let mut stmts = Vec::new();
        // S0: the header of the whole source.
        let alen = if let LTy::Arr(_, len) = p.ty {
            stmts.push(Stmt::Store { addr: slot_expr(hdr), off: 0, value: addr_of(&p) });
            let c = Expr { ty: Ty::U64, kind: ExprKind::Const(len as i128) };
            stmts.push(Stmt::Store { addr: slot_expr(hdr), off: 8, value: c });
            Some(len)
        } else {
            stmts.push(Stmt::Copy { dst: slot_expr(hdr), src: addr_of(&p), size: 16 });
            None
        };
        // S1: the start.
        let i = self.expr(start)?;
        let i = self.coerce(i, Ty::U64)?;
        let ci = if let ExprKind::Const(c) = i.kind { Some(c) } else { None };
        stmts.push(Stmt::Store { addr: slot_expr(scr), off: 0, value: i });
        let one = Expr { ty: Ty::U64, kind: ExprKind::Const(1) };
        let len_plus_1 = |l: Expr| Expr {
            ty: Ty::U64,
            kind: ExprKind::Arith { op: ArithOp::AddW, lhs: Box::new(l), rhs: Box::new(one.clone()), site: 0 },
        };
        // S2: the end, checked against the length, becomes the length.
        let mut cj = alen.map(|l| l as i128);
        if let Some(end) = end {
            let j = self.expr(end)?;
            let j = self.coerce(j, Ty::U64)?;
            cj = if let ExprKind::Const(c) = j.kind { Some(c) } else { None };
            if let (Some(c), Some(l)) = (cj, alen) {
                if c > l as i128 {
                    return self.reject(construct, format!("end {} out of bounds for `{}`", c, tname));
                }
            }
            let site = self.site(TrapKind::Bounds, format!("end of a slice of {}", tname), Ty::U64);
            let len = len_plus_1(at(hdr, 8, Ty::U64));
            let checked = Expr { ty: Ty::U64, kind: ExprKind::Bounds { idx: Box::new(j), len: Box::new(len), site } };
            stmts.push(Stmt::Store { addr: slot_expr(hdr), off: 8, value: checked });
        }
        if let (Some(a), Some(b)) = (ci, cj) {
            if a > b {
                return self.reject(construct, format!("start {} is past end {} in a slice of `{}`", a, b, tname));
            }
        }
        // S3: start <= end.
        let site = self.site(TrapKind::Bounds, format!("start of a slice of {}", tname), Ty::U64);
        let len = len_plus_1(at(hdr, 8, Ty::U64));
        let checked = Expr { ty: Ty::U64, kind: ExprKind::Bounds { idx: Box::new(at(scr, 0, Ty::U64)), len: Box::new(len), site } };
        stmts.push(Stmt::Eval(checked));
        // S4, S5: advance the pointer, shorten the length.
        let ptr = Expr {
            ty: Ty::Ptr,
            kind: ExprKind::Offset { base: Box::new(at(hdr, 0, Ty::Ptr)), idx: Box::new(at(scr, 0, Ty::U64)), scale: esize },
        };
        stmts.push(Stmt::Store { addr: slot_expr(hdr), off: 0, value: ptr });
        let rest = Expr {
            ty: Ty::U64,
            kind: ExprKind::Arith {
                op: ArithOp::SubW,
                lhs: Box::new(at(hdr, 8, Ty::U64)),
                rhs: Box::new(at(scr, 0, Ty::U64)),
                site: 0,
            },
        };
        stmts.push(Stmt::Store { addr: slot_expr(hdr), off: 8, value: rest });
        let addr = Expr { ty: Ty::Ptr, kind: ExprKind::Seq { stmts, value: Box::new(slot_expr(hdr)) } };
        Ok(Val::M(Place { addr, off: 0, ty: rt, mutable: false, temp: Some(hdr) }))
    }

    /// `base.name`: the place of a field (or of `p.*`), or, for `.len` of
    /// an array, its value.
    fn member(&mut self, n: &Node) -> R<Result<Place, Val>> {
        if n.children.len() != 1 {
            return self.reject("ExprFieldAccess", "unexpected shape".into());
        }
        let base = &n.children[0];
        if n.name == "*" {
            return match self.expr(base)? {
                Val::P(e, LTy::Ptr(inner, m)) => {
                    if let LTy::Struct(id) = *inner {
                        self.layout(id)?;
                    }
                    Ok(Ok(Place { addr: e, off: 0, ty: *inner, mutable: m, temp: None }))
                }
                Val::Poison => Err(()),
                _ => self.reject("ExprFieldAccess(.*)", "dereference of a non-pointer".into()),
            };
        }
        // `Color.red`, `std.math`: the base is not a value.
        if base.kind == NodeKind::ExprIdentifier
            && self.lookup(&base.name).is_none()
            && !self.const_nodes.contains_key(&base.name)
        {
            if self.enum_nodes.contains_key(&base.name) {
                let Some(id) = self.enum_id(&base.name)? else { unreachable!() };
                return self.enum_value(id, &n.name).map(Err);
            }
            if self.recover && self.poison_names.contains(&base.name) {
                return Err(());
            }
            return self.reject("ExprFieldAccess", format!("`{}.{}`", base.name, n.name));
        }
        let p = match self.expr(base)? {
            Val::M(p) => p,
            Val::Poison => return Err(()),
            Val::A(LTy::Arr(_, len), _) if n.name == "len" => {
                return Ok(Err(Val::E(Expr { ty: Ty::U64, kind: ExprKind::Const(len as i128) })))
            }
            Val::A(t, elems) => self.materialize(t, elems)?,
            // Field access through a pointer dereferences it.
            Val::P(e, LTy::Ptr(inner, m)) if is_agg(&inner) => {
                Place { addr: e, off: 0, ty: *inner, mutable: m, temp: None }
            }
            v @ Val::S(..) => match self.coerce_to(v, &LTy::Str)? {
                Val::M(p) => p,
                _ => return Err(()),
            },
            _ => return self.reject("ExprFieldAccess", format!("`.{}` on a value that is not a struct", n.name)),
        };
        if matches!(p.ty, LTy::Str | LTy::Slice(..)) {
            if n.name != "len" {
                let (c, t) = if p.ty == LTy::Str { ("ExprFieldAccess(str)", "str".to_string()) } else { ("ExprFieldAccess(slice)", self.type_name(&p.ty)) };
                return self.reject(c, format!("`.{}` of a {}", n.name, t));
            }
            // `.len` is read-only here: a str is never resized in place.
            return Ok(Ok(Place { addr: p.addr, off: p.off + 8, ty: LTy::S(Ty::U64), mutable: false, temp: None }));
        }
        if let LTy::Arr(_, len) = p.ty {
            if n.name != "len" {
                return self.reject("ExprFieldAccess", format!("`.{}` of an array", n.name));
            }
            // A compile-time constant; the array is still evaluated.
            let c = Expr { ty: Ty::U64, kind: ExprKind::Const(len as i128) };
            if pure_addr(&p.addr) {
                return Ok(Err(Val::E(c)));
            }
            let kind = ExprKind::Seq { stmts: vec![Stmt::Eval(p.addr)], value: Box::new(c) };
            return Ok(Err(Val::E(Expr { ty: Ty::U64, kind })));
        }
        let LTy::Struct(id) = p.ty else { unreachable!() };
        let fields = self.fields(id)?;
        match fields.iter().find(|f| f.name == n.name) {
            Some(f) => {
                // A field of a temporary is read from it only once.
                let mut q = field_place(&p, f);
                q.mutable = p.mutable && p.temp.is_none();
                Ok(Ok(q))
            }
            None => {
                let s = self.structs[id as usize].name.clone();
                self.reject("ExprFieldAccess", format!("`{}` has no field `{}`", s, n.name))
            }
        }
    }

    /// `dst op= rhs` for any place.
    fn store(&mut self, mut dst: Place, op: &str, rhs: &Node, out: &mut Vec<Stmt>) -> R<()> {
        if !dst.mutable {
            return self.reject("StmtAssign", "assignment through a constant".into());
        }
        let plain = op.is_empty() || op == "=";
        if plain {
            return self.init(rhs, dst, false, out);
        }
        let LTy::S(ty) = dst.ty else {
            let d = if matches!(dst.ty, LTy::Enum(..)) { "an enum" } else { "a pointer or a struct" };
            return self.reject("StmtAssign", format!("`{}` on {}", op, d));
        };
        let bin = match op.strip_suffix('=') {
            Some(b) if !b.is_empty() => b.to_string(),
            _ => return self.reject("StmtAssign", format!("operator `{}`", op)),
        };
        // The address is computed once, for both the load and the store.
        if !pure_addr(&dst.addr) {
            let h = self.hidden_var("%addr", LTy::Ptr(Box::new(dst.ty.clone()), true));
            out.push(Stmt::Assign { var: h, value: dst.addr });
            dst.addr = Expr { ty: Ty::Ptr, kind: ExprKind::Var(h) };
        }
        let cur = self.place_value(dst.clone())?;
        let r = self.expr(rhs)?;
        let v = self.binary(&bin, cur, r)?;
        let value = self.coerce(v, ty)?;
        out.push(Stmt::Store { addr: dst.addr, off: dst.off, value });
        Ok(())
    }

    /// A module-level struct constant: its bytes in read-only data.
    fn rodata(&mut self, n: &Node, t: LTy) -> R<Val> {
        if n.kind != NodeKind::ExprStructLit && n.kind != NodeKind::ExprArrayLiteral {
            let v = self.expr_as(n, &t)?;
            return match v {
                Val::M(p) if matches!(p.addr.kind, ExprKind::Data(_)) => Ok(Val::M(p)),
                Val::Poison => Err(()),
                _ => self.reject("ConstDecl", "struct or array constant is not a compile-time value".into()),
            };
        }
        let (size, _) = self.size_align(&t)?;
        let mut buf = vec![0u8; size as usize];
        self.const_fill(n, &t, &mut buf, 0)?;
        self.data.push(buf);
        let k = (self.data.len() - 1) as u32;
        Ok(Val::M(Place { addr: Expr { ty: Ty::Ptr, kind: ExprKind::Data(k) }, off: 0, ty: t, mutable: false, temp: None }))
    }

    /// A module-level array constant with strings in it: `Val::A`.
    fn const_array(&mut self, n: &Node, t: &LTy) -> R<Val> {
        self.see(n);
        let LTy::Arr(elem, _) = t else { unreachable!() };
        if n.kind != NodeKind::ExprArrayLiteral {
            let v = self.expr(n)?;
            return match v {
                Val::A(ref u, _) if u == t => Ok(v),
                Val::Poison => Err(()),
                _ => {
                    let a = self.type_name(t);
                    self.reject("ConstDecl", format!("`{}` constant is not a compile-time value", a))
                }
            };
        }
        self.array_count(n, t)?;
        let mut elems = Vec::new();
        for c in &n.children {
            let v = if **elem == LTy::Str {
                self.see(c);
                match self.expr(c)? {
                    v @ Val::S(..) => v,
                    Val::Poison => return Err(()),
                    _ => return self.reject("ConstDecl", "array element is not a string literal".into()),
                }
            } else {
                self.const_array(c, elem)?
            };
            elems.push(v);
        }
        Ok(Val::A(t.clone(), elems))
    }

    /// An array literal's element count must be the array's length.
    fn array_count(&mut self, n: &Node, t: &LTy) -> R<()> {
        let LTy::Arr(_, len) = t else { unreachable!() };
        if n.children.len() != *len as usize {
            let a = self.type_name(t);
            return self.reject(
                "ExprArrayLiteral",
                format!("{} elements for `{}`", n.children.len(), a),
            );
        }
        Ok(())
    }

    fn const_fill(&mut self, n: &Node, t: &LTy, buf: &mut [u8], off: usize) -> R<()> {
        self.see(n);
        if is_undefined(n) {
            return Ok(());
        }
        match t {
            LTy::S(ty) => {
                let v = self.expr(n)?;
                let e = self.coerce(v, *ty)?;
                let ExprKind::Const(c) = e.kind else {
                    return self.reject("ConstDecl", "struct field is not a compile-time value".into());
                };
                let raw = c as u64;
                for i in 0..ty.bytes() as usize {
                    buf[off + i] = (raw >> (8 * i)) as u8;
                }
                Ok(())
            }
            LTy::Enum(_, ty) => {
                let v = self.expr_as(n, t)?;
                let Val::P(Expr { kind: ExprKind::Const(c), .. }, _) = v else {
                    return self.reject("ConstDecl", "enum field is not a compile-time value".into());
                };
                let raw = c as u64;
                for i in 0..ty.bytes() as usize {
                    buf[off + i] = (raw >> (8 * i)) as u8;
                }
                Ok(())
            }
            LTy::Ptr(..) => self.reject("ConstDecl", "pointer in a constant struct".into()),
            LTy::Str => self.reject("ConstDecl(str field)", "str field in a module-level struct constant".into()),
            LTy::Slice(..) => self.reject("ConstDecl(slice)", "slice in a module-level constant".into()),
            LTy::Struct(id) if n.kind == NodeKind::ExprStructLit => {
                self.lit_type(n, t)?;
                let fields = self.fields(*id)?;
                let sname = self.structs[*id as usize].name.clone();
                let mut seen = vec![false; fields.len()];
                for c in &n.children {
                    if c.kind != NodeKind::ExprFieldAccess || c.children.len() != 1 {
                        return self.reject("ExprStructLit", format!("positional initializer in a `{}` literal", sname));
                    }
                    let Some(i) = fields.iter().position(|f| f.name == c.name) else {
                        return self.reject("ExprStructLit", format!("`{}` has no field `{}`", sname, c.name));
                    };
                    if seen[i] {
                        return self.reject("ExprStructLit", format!("field `{}` initialised twice", c.name));
                    }
                    seen[i] = true;
                    self.const_fill(&c.children[0], &fields[i].ty, buf, off + fields[i].off as usize)?;
                }
                for (i, f) in fields.iter().enumerate() {
                    if seen[i] {
                        continue;
                    }
                    let Some(d) = f.default else {
                        return self.reject("ExprStructLit", format!("missing field `{}` of `{}`", f.name, sname));
                    };
                    self.const_fill(d, &f.ty, buf, off + f.off as usize)?;
                }
                Ok(())
            }
            LTy::Arr(elem, _) if n.kind == NodeKind::ExprArrayLiteral => {
                self.array_count(n, t)?;
                let (esize, _) = self.size_align(elem)?;
                for (i, c) in n.children.iter().enumerate() {
                    self.const_fill(c, elem, buf, off + i * esize as usize)?;
                }
                Ok(())
            }
            LTy::Struct(_) | LTy::Arr(..) => {
                let v = self.expr_as(n, t)?;
                let Val::M(p) = v else { return Err(()) };
                let ExprKind::Data(k) = p.addr.kind else {
                    return self.reject("ConstDecl", "struct field is not a compile-time value".into());
                };
                let (size, _) = self.size_align(t)?;
                let src = &self.data[k as usize][p.off as usize..(p.off + size) as usize];
                buf[off..off + size as usize].copy_from_slice(src);
                Ok(())
            }
        }
    }
}

/// The register type of a scalar or pointer; None for a struct.
fn reg_ty(t: &LTy) -> Option<Ty> {
    match t {
        LTy::S(ty) => Some(*ty),
        LTy::Ptr(..) => Some(Ty::Ptr),
        LTy::Enum(_, ty) => Some(*ty),
        LTy::Struct(_) | LTy::Str | LTy::Arr(..) | LTy::Slice(..) => None,
    }
}

/// Lives in memory and is passed by reference: a struct, a `str`, an
/// array or a slice.
fn is_agg(t: &LTy) -> bool {
    matches!(t, LTy::Struct(_) | LTy::Str | LTy::Arr(..) | LTy::Slice(..))
}

/// An array type with strings at its leaves: no read-only image of it can
/// exist, so its module constants are `Val::A`.
fn holds_str(t: &LTy) -> bool {
    match t {
        LTy::Str => true,
        LTy::Arr(inner, _) => holds_str(inner),
        _ => false,
    }
}

/// The place of element `i` (a constant) of array place `p`.
fn elem_place(p: &Place, elem: &LTy, esize: u32, i: u32) -> Place {
    Place { addr: p.addr.clone(), off: p.off + i * esize, ty: elem.clone(), mutable: p.mutable, temp: None }
}

/// `__t27b_str_eql(a: *const str, b: *const str) bool`: equal lengths and
/// equal bytes, what `std.mem.eql(u8, a, b)` computes.
fn str_eql_func(noreturn_site: SiteId) -> Func {
    let var = |id: VarId, ty: Ty| Expr { ty, kind: ExprKind::Var(id) };
    let cnst = |ty: Ty, v: i128| Expr { ty, kind: ExprKind::Const(v) };
    let load = |addr: Expr, off: u32, ty: Ty| Expr { ty, kind: ExprKind::Load { addr: Box::new(addr), off } };
    let ne = |a: Expr, b: Expr| Expr { ty: Ty::Bool, kind: ExprKind::Cmp { op: CmpOp::Ne, lhs: Box::new(a), rhs: Box::new(b) } };
    let byte = |s: VarId| {
        let base = load(var(s, Ty::Ptr), 0, Ty::Ptr);
        let at = Expr {
            ty: Ty::Ptr,
            kind: ExprKind::Offset { base: Box::new(base), idx: Box::new(var(3, Ty::U64)), scale: 1 },
        };
        load(at, 0, Ty::U8)
    };
    let (a, b, n, i) = (0, 1, 2, 3);
    let body = vec![
        Stmt::Assign { var: n, value: load(var(a, Ty::Ptr), 8, Ty::U64) },
        Stmt::If {
            cond: ne(load(var(b, Ty::Ptr), 8, Ty::U64), var(n, Ty::U64)),
            then: vec![Stmt::Return(Some(cnst(Ty::Bool, 0)))],
            els: vec![],
        },
        Stmt::Assign { var: i, value: cnst(Ty::U64, 0) },
        Stmt::While {
            cond: Expr {
                ty: Ty::Bool,
                kind: ExprKind::Cmp { op: CmpOp::Lt, lhs: Box::new(var(i, Ty::U64)), rhs: Box::new(var(n, Ty::U64)) },
            },
            body: vec![Stmt::If {
                cond: ne(byte(a), byte(b)),
                then: vec![Stmt::Return(Some(cnst(Ty::Bool, 0)))],
                els: vec![],
            }],
            // i < n <= 2^64 - 1, so i + 1 cannot wrap.
            step: vec![Stmt::Assign {
                var: i,
                value: Expr {
                    ty: Ty::U64,
                    kind: ExprKind::Arith {
                        op: ArithOp::AddW,
                        lhs: Box::new(var(i, Ty::U64)),
                        rhs: Box::new(cnst(Ty::U64, 1)),
                        site: 0,
                    },
                },
            }],
        },
        Stmt::Return(Some(cnst(Ty::Bool, 1))),
    ];
    let v = |name: &str, ty: Ty| Var { name: name.to_string(), ty };
    Func {
        name: STR_EQL.to_string(),
        nparams: 2,
        ret: Some(Ty::Bool),
        vars: vec![v("a", Ty::Ptr), v("b", Ty::Ptr), v("n", Ty::U64), v("i", Ty::U64)],
        body,
        line: 0,
        is_test: false,
        is_invariant: false,
        noreturn_site,
        slots: Vec::new(),
    }
}

/// The one function lowering synthesizes; not a name t27 source can declare
/// (`__t27b_` is reserved to the backend).
pub const STR_EQL: &str = "__t27b_str_eql";

fn val_of(e: Expr, t: &LTy) -> Val {
    match t {
        LTy::S(_) => Val::E(e),
        _ => Val::P(e, t.clone()),
    }
}

fn slot_expr(k: u32) -> Expr {
    Expr { ty: Ty::Ptr, kind: ExprKind::Slot(k) }
}

fn addr_of(p: &Place) -> Expr {
    if p.off == 0 {
        return p.addr.clone();
    }
    let idx = Expr { ty: Ty::U64, kind: ExprKind::Const(p.off as i128) };
    Expr { ty: Ty::Ptr, kind: ExprKind::Offset { base: Box::new(p.addr.clone()), idx: Box::new(idx), scale: 1 } }
}

fn field_place(p: &Place, f: &Field) -> Place {
    Place { addr: p.addr.clone(), off: p.off + f.off, ty: f.ty.clone(), mutable: p.mutable, temp: None }
}

/// An address that may be evaluated more than once with the same result and
/// no effect.
fn pure_addr(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Slot(_) | ExprKind::Data(_) | ExprKind::Var(_) => true,
        ExprKind::Offset { base, idx, .. } => pure_addr(base) && matches!(idx.kind, ExprKind::Const(_)),
        _ => false,
    }
}

fn is_undefined(n: &Node) -> bool {
    n.kind == NodeKind::ExprIdentifier && n.name == "undefined"
}

/// Names whose address is taken (`&name`) anywhere in a body.
fn scan_addr_taken(ns: &[Node], out: &mut HashSet<String>) {
    for n in ns {
        if n.kind == NodeKind::ExprUnary && n.extra_op == "&" {
            if let Some(c) = n.children.first() {
                if c.kind == NodeKind::ExprIdentifier {
                    out.insert(c.name.clone());
                }
            }
        }
        scan_addr_taken(&n.children, out);
    }
}

/// Count plain assignments per identifier in a test body (all nesting levels).
fn count_assigns(ns: &[Node], counts: &mut HashMap<String, u32>) {
    for n in ns {
        if n.kind == NodeKind::StmtAssign {
            if let Some(t) = n.children.first() {
                if t.kind == NodeKind::ExprIdentifier {
                    *counts.entry(t.name.clone()).or_insert(0) += 1;
                }
            }
        }
        count_assigns(&n.children, counts);
    }
}
