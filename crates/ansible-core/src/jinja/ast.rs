//! The expression tree, shaped like jinja2 3.1.6's `nodes.py`.
//!
//! Node names and field names are upstream's, so "does ours agree with theirs" stays a
//! side-by-side read of two files rather than an argument — and so the differential dumper
//! can walk `Node.fields` generically instead of hand-writing a case per node.
//!
//! What is deliberately *not* upstream:
//!
//! - **Every node carries a [`Span`].** `nodes.py` keeps `lineno` and nothing else, which
//!   cannot answer "what is under the cursor". Spans are excluded when comparing against
//!   upstream, so they need their own tests.
//! - **`ctx` is dropped.** Upstream tags `Name`, `Getattr`, `Getitem` and `Tuple` with
//!   `load`/`store`/`param`; only `load` is reachable from `parse_expression`, and a field
//!   with one possible value is noise.
//! - **The codegen-only nodes are absent** — `NSRef`, `InternalName`, `EnvironmentAttribute`,
//!   `ContextReference` and friends. None is reachable from source text.

use crate::parse::Span;

#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    /// A literal, already decoded — `'a'`, `1`, `1.5`, `true`, `none`.
    Const(Const),
    /// A variable to look up. `true`/`false`/`none` lex as names but never reach here;
    /// `parse_primary` turns them into [`ExprKind::Const`], as upstream does.
    Name(String),
    List(Vec<Expr>),
    /// Only ever produced by parentheses — a bare `1, 2` is not an expression, it is an
    /// expression followed by a stray comma.
    Tuple(Vec<Expr>),
    Dict(Vec<(Expr, Expr)>),
    /// `r.stdout`. A `.name` accessor; `.0` becomes a [`ExprKind::Getitem`] instead.
    Getattr { node: Box<Expr>, attr: String },
    /// `r['stdout']`, `r[0]`, `hostvars[h]` — the arg is an arbitrary expression, which is
    /// what makes a non-literal subscript representable at all.
    Getitem { node: Box<Expr>, arg: Box<Expr> },
    Slice { start: Option<Box<Expr>>, stop: Option<Box<Expr>>, step: Option<Box<Expr>> },
    /// `x | default('v')`. `name` may be dotted — `ansible.builtin.length`.
    Filter { node: Box<Expr>, name: String, args: Args },
    /// `x is defined`. `is not defined` is this wrapped in [`UnOp::Not`], exactly as
    /// `parse_test` builds it.
    Test { node: Box<Expr>, name: String, args: Args },
    Call { node: Box<Expr>, args: Args },
    /// Chained, because `1 < 2 < 3` is one comparison with two operands upstream.
    Compare { expr: Box<Expr>, ops: Vec<(CmpOp, Expr)> },
    /// `'a' ~ b ~ 'c'` — n-ary, not a fold of binary nodes.
    Concat(Vec<Expr>),
    /// `a if b else c`; `else` is optional and yields `None` at render time.
    CondExpr { test: Box<Expr>, then: Box<Expr>, or_else: Option<Box<Expr>> },
    Bin { op: BinOp, left: Box<Expr>, right: Box<Expr> },
    Unary { op: UnOp, node: Box<Expr> },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Const {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    None,
}

/// `parse_call_args`' four buckets, shared by calls, filters and tests because upstream
/// shares them through `_FilterTestCommon`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Args {
    pub args: Vec<Expr>,
    pub kwargs: Vec<(String, Expr)>,
    /// `*rest`
    pub dyn_args: Option<Box<Expr>>,
    /// `**rest`
    pub dyn_kwargs: Option<Box<Expr>>,
}

impl Args {
    /// No arguments of any kind — `x is defined` rather than `x is divisibleby 3`.
    pub fn is_empty(&self) -> bool {
        self.args.is_empty()
            && self.kwargs.is_empty()
            && self.dyn_args.is_none()
            && self.dyn_kwargs.is_none()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    FloorDiv,
    Mod,
    Pow,
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Not,
    Neg,
    Pos,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Gt,
    Gteq,
    Lt,
    Lteq,
    In,
    NotIn,
}

impl BinOp {
    /// Upstream's class name, which is what the differential compares against.
    pub fn node_name(self) -> &'static str {
        match self {
            BinOp::Add => "Add",
            BinOp::Sub => "Sub",
            BinOp::Mul => "Mul",
            BinOp::Div => "Div",
            BinOp::FloorDiv => "FloorDiv",
            BinOp::Mod => "Mod",
            BinOp::Pow => "Pow",
            BinOp::And => "And",
            BinOp::Or => "Or",
        }
    }
}

impl UnOp {
    pub fn node_name(self) -> &'static str {
        match self {
            UnOp::Not => "Not",
            UnOp::Neg => "Neg",
            UnOp::Pos => "Pos",
        }
    }
}

impl CmpOp {
    /// `Operand.op` upstream: the token type name, except membership, which is spelled
    /// `in` / `notin` rather than after any token.
    pub fn op_name(self) -> &'static str {
        match self {
            CmpOp::Eq => "eq",
            CmpOp::Ne => "ne",
            CmpOp::Gt => "gt",
            CmpOp::Gteq => "gteq",
            CmpOp::Lt => "lt",
            CmpOp::Lteq => "lteq",
            CmpOp::In => "in",
            CmpOp::NotIn => "notin",
        }
    }
}

impl Expr {
    /// The root variable this expression reads, if it reads exactly one and reads it
    /// directly — walking the accessor spine to the `Name` at its base.
    ///
    /// `r.results[0].stdout` answers `r`. `hostvars[h].x` also answers `hostvars`, because
    /// the *subject* is `hostvars`; `h` is an argument to it and is not on the spine. This is
    /// what T-186 needed and could not express while a reference was a `String`.
    pub fn root_name(&self) -> Option<&str> {
        match &self.kind {
            ExprKind::Name(n) => Some(n),
            ExprKind::Getattr { node, .. } | ExprKind::Getitem { node, .. } => node.root_name(),
            _ => None,
        }
    }
}
