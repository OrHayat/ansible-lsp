//! Which names a template looks up in its render context, and where — T-212.
//!
//! A templated scalar is a whole template (`template.rs`'s header says why), so a name can be
//! bound by the template itself: `h` in `{% for h in groups.web %}{{ h }}{% endfor %}` is the
//! loop's, and looking it up in the workspace is the false "never defined" this module exists
//! to stop. The answer is Jinja's own scoping, walked over the parsed blocks.
//!
//! `jinja2.meta.find_undeclared_variables` is the differential oracle, and it is not the
//! truth everywhere. Measured against rendering with `StrictUndefined` on jinja2 3.1.6, it
//! over-reports in three shapes and under-reports in one:
//!
//! | template                                              | meta   | renders          |
//! | ----------------------------------------------------- | ------ | ---------------- |
//! | `{% if c %}{% set q=1 %}{{ q }}{% endif %}`           | `q`    | fine             |
//! | `{% if c %}{% set q=1 %}{% else %}{% set q=2 %}{% endif %}{{ q }}` | `q` | fine  |
//! | `{% set q=1 %}{% block b %}{{ q }}{% endblock %}`, and the same for a name `import`ed or `from`-imported at the top | `q` | fine |
//! | `{% set q %}{{ q }}{% endset %}`                      | —      | `'q' is undefined` |
//!
//! This follows the rendering. One more deliberate departure: a `set` inside an `if` leaves
//! the name bound after the `endif`, where rendering says "only if the branch ran". That is
//! the undefined rule's standing policy — any reachable definition exempts — applied inside
//! the template as it already is across tasks.
//!
//! A template this cannot read — one that does not split, does not nest, or uses a tag not
//! modelled here — is `None`, never a partial answer.

use std::collections::HashSet;

use super::ast::{Args, Expr, ExprKind};
use super::lexer::{self, Kind, Token};
use super::{parser, statement, template, Delimiters};
use crate::parse::Span;

/// One lookup of `name` in the render context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lookup {
    pub name: String,
    pub span: Span,
    /// The expression it sits in — a `{{ }}` body, or one expression of a statement — which
    /// parses on its own, so a guard check can read it.
    pub expr: Span,
    /// The callee of a call: `lookup` in `lookup('file', p)`.
    pub called: bool,
}

/// The context lookups of a template, in source order, every occurrence.
pub fn template_lookups(src: &str) -> Option<Vec<Lookup>> {
    let blocks = template::blocks(src, &Delimiters::default()).ok()?;
    statement::check(src, &blocks).ok()?;
    let mut w = Walk {
        src,
        frames: vec![Frame::new(Scope::Root)],
        assigned: HashSet::new(),
        out: Vec::new(),
    };
    // A macro body runs when it is called, which may be after a `set` further down — so
    // inside one, every name the template assigns anywhere counts as bound (measured:
    // `{% macro f() %}{{ q }}{% endmacro %}{% set q = 1 %}{{ f() }}` renders).
    w.assigned = assigned_anywhere(src, &blocks);
    for b in &blocks {
        match b.kind {
            template::Kind::Data | template::Kind::Comment => {}
            template::Kind::Expression => {
                let e = parser::parse(b.inner.slice(src)).ok()?;
                w.read(&e, b.inner.start, b.inner);
            }
            template::Kind::Statement => w.statement(b.inner)?,
        }
    }
    Some(w.out)
}

/// The context lookups of one bare expression — a `when:` clause.
pub fn expression_lookups(src: &str) -> Option<Vec<Lookup>> {
    let e = parser::parse(src).ok()?;
    let mut out = Vec::new();
    collect(&e, 0, Span { start: 0, end: src.len() }, &|_| false, false, &mut out);
    Some(out)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Root,
    /// Names set in one branch leak past `endif`; see the module doc.
    If,
    For,
    /// `with`, `filter`, `block`, `autoescape`: sees outward, nothing leaks out.
    Nested,
    /// `set x` … `endset`: `x` is bound after the body, not in it.
    SetBlock,
    /// `macro` and `call` bodies, which run when called.
    Deferred,
}

struct Frame {
    scope: Scope,
    names: HashSet<String>,
    /// An `if`'s earlier branches; a `set x` block's target.
    held: Vec<String>,
}

impl Frame {
    fn new(scope: Scope) -> Self {
        Frame { scope, names: HashSet::new(), held: Vec::new() }
    }
}

struct Walk<'a> {
    src: &'a str,
    frames: Vec<Frame>,
    assigned: HashSet<String>,
    out: Vec<Lookup>,
}

impl Walk<'_> {
    fn bound(&self, name: &str) -> bool {
        self.frames.iter().any(|f| f.names.contains(name))
            || (self.assigned.contains(name) && self.frames.iter().any(|f| f.scope == Scope::Deferred))
    }

    fn bind(&mut self, name: &str) {
        self.frames.last_mut().expect("root frame").names.insert(name.to_string());
    }

    fn push(&mut self, scope: Scope, names: impl IntoIterator<Item = String>) {
        let mut f = Frame::new(scope);
        f.names.extend(names);
        self.frames.push(f);
    }

    fn pop(&mut self) -> Option<Frame> {
        (self.frames.len() > 1).then(|| self.frames.pop()).flatten()
    }

    /// Record `e`'s lookups. `base` is where the text `e` was parsed from starts in `src`.
    fn read(&mut self, e: &Expr, base: usize, expr: Span) {
        let mut found = Vec::new();
        collect(e, base, expr, &|n| self.bound(n), false, &mut found);
        self.out.extend(found);
    }

    fn statement(&mut self, inner: Span) -> Option<()> {
        let body = inner.slice(self.src);
        let toks = lexer::tokens(body).ok()?;
        let base = inner.start;
        let word = |i: usize| name_at(&toks, i, body);
        let tag = word(0)?;
        let expr_at = |i: usize| parser::expression_at(body, &toks, i).ok();
        let span_of = |e: &Expr| Span { start: base + e.span.start, end: base + e.span.end };

        match tag {
            "for" => {
                let in_at = (1..toks.len()).find(|&i| depth_at(&toks, i) == 0 && word(i) == Some("in"))?;
                let targets = targets(&toks, 1, in_at, body)?;
                // The iterable is `parse_tuple(with_condexpr=False)` upstream: it stops at a
                // top-level `if` (the loop filter) or `recursive`, which `expression_at`
                // would read as a conditional expression.
                let stop = (in_at + 1..toks.len())
                    .find(|&i| depth_at(&toks, i) == 0 && matches!(word(i), Some("if" | "recursive")))
                    .unwrap_or(toks.len() - 1);
                let iter = cut(&toks, stop);
                let mut i = in_at + 1;
                loop {
                    let (e, next) = parser::expression_at(body, &iter, i).ok()?;
                    self.read(&e, base, span_of(&e));
                    i = next;
                    match iter[i].kind {
                        Kind::Comma => i += 1,
                        Kind::Eof => break,
                        _ => return None,
                    }
                }
                // The filter sees the targets and not `loop` — measured both ways:
                // `for i in xs if loop` is `'loop' is undefined`.
                self.push(Scope::For, targets);
                if word(stop) == Some("if") {
                    let (e, next) = expr_at(stop + 1)?;
                    self.read(&e, base, span_of(&e));
                    if !(toks[next].kind == Kind::Eof || word(next) == Some("recursive")) {
                        return None;
                    }
                }
                self.bind("loop");
            }
            "if" => {
                let (e, _) = expr_at(1)?;
                self.read(&e, base, span_of(&e));
                self.push(Scope::If, []);
            }
            "elif" => {
                self.close_branch()?;
                let (e, _) = expr_at(1)?;
                self.read(&e, base, span_of(&e));
            }
            "else" => match self.frames.last()?.scope {
                Scope::If => self.close_branch()?,
                // A `for`'s `else` runs when there was nothing to loop over: no targets, no
                // `loop` (measured).
                Scope::For => self.frames.last_mut()?.names.clear(),
                _ => return None,
            },
            "endif" => {
                let f = self.pop()?;
                for n in f.names.into_iter().chain(f.held) {
                    self.bind(&n);
                }
            }
            "set" => {
                let assign = (1..toks.len()).find(|&i| depth_at(&toks, i) == 0 && toks[i].kind == Kind::Assign);
                let end = assign
                    .or_else(|| (1..toks.len()).find(|&i| toks[i].kind == Kind::Pipe))
                    .unwrap_or(toks.len() - 1);
                // `set ns.attr = …` writes into a namespace that has to exist already.
                let writes_attr = toks[1..end].iter().any(|t| t.kind == Kind::Dot);
                let names = if writes_attr { Vec::new() } else { targets(&toks, 1, end, body)? };
                match assign {
                    Some(at) => {
                        self.read_list(&toks, at + 1, body, base)?;
                        if writes_attr {
                            let root = toks[1];
                            let name = root.span.slice(body);
                            if root.kind != Kind::Name {
                                return None;
                            }
                            if !self.bound(name) {
                                let span = Span { start: base + root.span.start, end: base + root.span.end };
                                self.out.push(Lookup { name: name.to_string(), span, expr: span, called: false });
                            }
                        }
                        for n in names {
                            self.bind(&n);
                        }
                    }
                    None => {
                        let mut f = Frame::new(Scope::SetBlock);
                        f.held = names;
                        self.frames.push(f);
                    }
                }
            }
            "endset" => {
                let f = self.pop()?;
                if f.scope != Scope::SetBlock {
                    return None;
                }
                for n in f.held {
                    self.bind(&n);
                }
            }
            "with" => {
                // Every value reads the outer scope, the later ones included: measured,
                // `with a = 1, b = a` reports `a`.
                let mut names = Vec::new();
                let mut i = 1;
                while toks[i].kind != Kind::Eof {
                    let n = word(i)?;
                    if toks[i + 1].kind != Kind::Assign {
                        return None;
                    }
                    let (e, next) = expr_at(i + 2)?;
                    self.read(&e, base, span_of(&e));
                    names.push(n.to_string());
                    i = next;
                    if toks[i].kind == Kind::Comma {
                        i += 1;
                    }
                }
                self.push(Scope::Nested, names);
            }
            "macro" => {
                let name = word(1)?;
                // Visible in its own body, so recursion is not a lookup.
                self.bind(name);
                self.push(Scope::Deferred, ["varargs", "kwargs", "caller"].map(String::from));
                let after = self.signature(&toks, 2, body, base)?;
                if toks[after].kind != Kind::Eof {
                    return None;
                }
            }
            "call" => {
                // `call(params) callee(args)`: the callee reads the outer scope, the params
                // are the body's.
                let params_end = if toks[1].kind == Kind::Lparen { close_of(&toks, 1)? + 1 } else { 1 };
                let (e, next) = expr_at(params_end)?;
                if toks[next].kind != Kind::Eof {
                    return None;
                }
                self.read(&e, base, span_of(&e));
                self.push(Scope::Deferred, []);
                if params_end > 1 {
                    self.signature(&toks, 1, body, base)?;
                }
            }
            "filter" => {
                // The filter section is a filter chain with nothing on its left. Parse it with
                // a stand-in subject and keep only what its arguments read.
                let rest_at = toks[1].span.start;
                let text = format!("_|{}", &body[rest_at..]);
                let e = parser::parse(&text).ok()?;
                let shift = base + rest_at - 2;
                let mut found = Vec::new();
                collect(&e, shift, Span { start: base + rest_at, end: base + body.len() }, &|n| self.bound(n), false, &mut found);
                self.out.extend(found.into_iter().filter(|l| l.span.start >= base + rest_at));
                self.push(Scope::Nested, []);
            }
            "block" => self.push(Scope::Nested, ["super".to_string()]),
            "autoescape" => {
                let (e, _) = expr_at(1)?;
                self.read(&e, base, span_of(&e));
                self.push(Scope::Nested, []);
            }
            "endfor" | "endwith" | "endfilter" | "endblock" | "endautoescape" | "endmacro" | "endcall" => {
                self.pop()?;
            }
            "import" => {
                let (e, next) = expr_at(1)?;
                self.read(&e, base, span_of(&e));
                if word(next) != Some("as") {
                    return None;
                }
                let n = word(next + 1)?;
                self.bind(n);
            }
            "from" => {
                let (e, next) = expr_at(1)?;
                self.read(&e, base, span_of(&e));
                if word(next) != Some("import") {
                    return None;
                }
                let mut i = next + 1;
                while let Some(n) = word(i) {
                    if matches!(n, "with" | "without") {
                        break;
                    }
                    // `b as c` lands as `c`.
                    let landed = if word(i + 1) == Some("as") {
                        i += 2;
                        word(i)?
                    } else {
                        n
                    };
                    self.bind(landed);
                    i += 1;
                    if toks[i].kind == Kind::Comma {
                        i += 1;
                    }
                }
            }
            "include" | "extends" | "print" | "do" => {
                let (e, _) = expr_at(1)?;
                self.read(&e, base, span_of(&e));
            }
            _ => return None,
        }
        Some(())
    }

    /// An `elif` or `else`: what this branch set is held for after `endif`, not visible to
    /// the next branch.
    fn close_branch(&mut self) -> Option<()> {
        let f = self.frames.last_mut()?;
        if f.scope != Scope::If {
            return None;
        }
        let names: Vec<String> = f.names.drain().collect();
        f.held.extend(names);
        Some(())
    }

    /// Comma-separated expressions from `i` to the end of the statement.
    fn read_list(&mut self, toks: &[Token], mut i: usize, body: &str, base: usize) -> Option<()> {
        loop {
            let (e, next) = parser::expression_at(body, toks, i).ok()?;
            self.read(&e, base, Span { start: base + e.span.start, end: base + e.span.end });
            match toks[next].kind {
                Kind::Comma => i = next + 1,
                Kind::Eof => return Some(()),
                _ => return None,
            }
        }
    }

    /// `(a, b=a, c)` from the `(` at `open`, binding each parameter into the current frame.
    /// A default is read before its own parameter is bound and after the earlier ones —
    /// measured: `macro f(a, b=a)` reports nothing. Returns the index past the `)`.
    fn signature(&mut self, toks: &[Token], open: usize, body: &str, base: usize) -> Option<usize> {
        if toks[open].kind != Kind::Lparen {
            return Some(open);
        }
        let mut i = open + 1;
        while toks[i].kind != Kind::Rparen {
            let n = name_at(toks, i, body)?.to_string();
            i += 1;
            if toks[i].kind == Kind::Assign {
                let (e, next) = parser::expression_at(body, toks, i + 1).ok()?;
                self.read(&e, base, Span { start: base + e.span.start, end: base + e.span.end });
                i = next;
            }
            self.bind(&n);
            match toks[i].kind {
                Kind::Comma => i += 1,
                Kind::Rparen => {}
                _ => return None,
            }
        }
        Some(i + 1)
    }
}

/// Every name a statement anywhere in the template assigns — `set` targets, macro names,
/// imported names. What a macro body may read, since it runs later, and what the
/// differential lets jinja2's meta over-report (the module doc's table).
fn assigned_anywhere(src: &str, blocks: &[template::Block]) -> HashSet<String> {
    let mut out = HashSet::new();
    for b in blocks.iter().filter(|b| b.kind == template::Kind::Statement) {
        let body = b.inner.slice(src);
        let Ok(toks) = lexer::tokens(body) else { continue };
        match name_at(&toks, 0, body) {
            Some("set") => {
                let end = (1..toks.len())
                    .find(|&i| matches!(toks[i].kind, Kind::Assign | Kind::Pipe | Kind::Dot))
                    .unwrap_or(toks.len() - 1);
                out.extend(targets(&toks, 1, end, body).unwrap_or_default());
            }
            Some("macro") => out.extend(name_at(&toks, 1, body).map(String::from)),
            Some("import") => {
                if let Some(i) = (1..toks.len()).find(|&i| name_at(&toks, i, body) == Some("as")) {
                    out.extend(name_at(&toks, i + 1, body).map(String::from));
                }
            }
            Some("from") => {
                let Some(start) = (1..toks.len()).find(|&i| name_at(&toks, i, body) == Some("import")) else {
                    continue;
                };
                let mut i = start + 1;
                while let Some(n) = name_at(&toks, i, body) {
                    if matches!(n, "with" | "without") {
                        break;
                    }
                    if name_at(&toks, i + 1, body) == Some("as") {
                        i += 2;
                    }
                    out.extend(name_at(&toks, i, body).map(String::from));
                    i += 1;
                    if toks.get(i).is_some_and(|t| t.kind == Kind::Comma) {
                        i += 1;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn name_at<'b>(toks: &[Token], i: usize, body: &'b str) -> Option<&'b str> {
    toks.get(i).filter(|t| t.kind == Kind::Name).map(|t| t.span.slice(body))
}

/// Bracket depth just before token `i`.
fn depth_at(toks: &[Token], i: usize) -> i32 {
    toks[..i].iter().fold(0, |d, t| match t.kind {
        Kind::Lparen | Kind::Lbracket | Kind::Lbrace => d + 1,
        Kind::Rparen | Kind::Rbracket | Kind::Rbrace => d - 1,
        _ => d,
    })
}

/// The `)` matching the `(` at `open`.
fn close_of(toks: &[Token], open: usize) -> Option<usize> {
    (open + 1..toks.len()).find(|&i| toks[i].kind == Kind::Rparen && depth_at(toks, i) == 1)
}

/// The tokens up to `stop`, ended there.
fn cut(toks: &[Token], stop: usize) -> Vec<Token> {
    let at = toks[stop].span.start;
    let mut v = toks[..stop].to_vec();
    v.push(Token { kind: Kind::Eof, span: Span { start: at, end: at } });
    v
}

/// An assignment target — names, commas and parentheses only — as its names.
fn targets(toks: &[Token], from: usize, to: usize, body: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for t in &toks[from..to] {
        match t.kind {
            Kind::Name => out.push(t.span.slice(body).to_string()),
            Kind::Comma | Kind::Lparen | Kind::Rparen => {}
            _ => return None,
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Every `Name` in `e` that `bound` does not claim. Filter and test names, attribute names
/// and keyword-argument names are not `Name` nodes, so they never get here.
fn collect(
    e: &Expr,
    base: usize,
    expr: Span,
    bound: &impl Fn(&str) -> bool,
    called: bool,
    out: &mut Vec<Lookup>,
) {
    macro_rules! walk {
        ($x:expr) => {
            collect($x, base, expr, bound, false, out)
        };
    }
    match &e.kind {
        ExprKind::Name(n) => {
            if !bound(n) {
                let span = Span { start: base + e.span.start, end: base + e.span.end };
                out.push(Lookup { name: n.clone(), span, expr, called });
            }
        }
        ExprKind::Const(_) => {}
        ExprKind::List(xs) | ExprKind::Tuple(xs) | ExprKind::Concat(xs) => {
            for x in xs {
                walk!(x);
            }
        }
        ExprKind::Dict(kv) => {
            for (k, v) in kv {
                walk!(k);
                walk!(v);
            }
        }
        ExprKind::Getattr { node, .. } => walk!(node),
        ExprKind::Getitem { node, arg } => {
            walk!(node);
            walk!(arg);
        }
        ExprKind::Slice { start, stop, step } => {
            for x in [start, stop, step].into_iter().flatten() {
                walk!(x);
            }
        }
        ExprKind::Filter { node, args, .. } | ExprKind::Test { node, args, .. } => {
            walk!(node);
            args_of(args, &mut |x| collect(x, base, expr, bound, false, out));
        }
        ExprKind::Call { node, args } => {
            collect(node, base, expr, bound, true, out);
            args_of(args, &mut |x| collect(x, base, expr, bound, false, out));
        }
        ExprKind::Compare { expr: first, ops } => {
            walk!(first);
            for (_, x) in ops {
                walk!(x);
            }
        }
        ExprKind::CondExpr { test, then, or_else } => {
            walk!(then);
            walk!(test);
            if let Some(x) = or_else {
                walk!(x);
            }
        }
        ExprKind::Bin { left, right, .. } => {
            walk!(left);
            walk!(right);
        }
        ExprKind::Unary { node, .. } => walk!(node),
    }
}

fn args_of(a: &Args, go: &mut impl FnMut(&Expr)) {
    a.args.iter().for_each(&mut *go);
    a.kwargs.iter().for_each(|(_, x)| go(x));
    if let Some(x) = &a.dyn_args {
        go(x);
    }
    if let Some(x) = &a.dyn_kwargs {
        go(x);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// jinja2's `DEFAULT_NAMESPACE` globals. `find_undeclared_variables` resolves them from
    /// the environment and never lists them; a lookup here is still a lookup.
    const GLOBALS: &[&str] = &["range", "dict", "lipsum", "cycler", "joiner", "namespace"];

    fn names(src: &str) -> Vec<String> {
        template_lookups(src).expect("reads").into_iter().map(|l| l.name).collect()
    }

    /// Rows of a `scripts/jinja_names.py` corpus that break the differential rule, and how
    /// many were compared.
    ///
    /// Equal name sets, except in the shapes the module doc measures meta getting wrong: a
    /// name meta lists and we do not must be one the template itself binds (the `if` and
    /// `block` rows), and a name we list and meta does not is only a `set` block reading its
    /// own target, which rendering fails on. A row jinja2 parses and we decline must be one
    /// our own statement checker refuses — never a template that simply went unread.
    fn differential(corpus: &str) -> (usize, Vec<String>) {
        let (mut compared, mut bad) = (0, Vec::new());
        let short = |s: &str| s.chars().take(100).collect::<String>();
        for line in corpus.lines().filter(|l| !l.trim().is_empty()) {
            let row: Value = serde_json::from_str(line).unwrap();
            let src = row["src"].as_str().unwrap();
            let Some(meta) = row["meta"].as_array() else { continue };
            let meta: HashSet<String> = meta.iter().map(|v| v.as_str().unwrap().to_string()).collect();
            let Some(ours) = template_lookups(src) else {
                let refused = template::blocks(src, &Delimiters::default())
                    .map_or(true, |bs| statement::check(src, &bs).is_err());
                if !refused {
                    bad.push(format!("UNREAD {:?}", short(src)));
                }
                continue;
            };
            let ours: HashSet<String> =
                ours.into_iter().map(|l| l.name).filter(|n| !GLOBALS.contains(&n.as_str())).collect();
            compared += 1;
            let blocks = template::blocks(src, &Delimiters::default()).unwrap();
            let bound_here = assigned_anywhere(src, &blocks);
            for n in meta.difference(&ours).filter(|n| !bound_here.contains(*n)) {
                bad.push(format!("MISSED {n:?} in {:?}", short(src)));
            }
            for n in ours.difference(&meta) {
                let own_body = src.contains(&format!("{{% set {n} %}}")) && src.contains(&format!("{{{{ {n} }}}}"));
                if !own_body {
                    bad.push(format!("EXTRA {n:?} in {:?}", short(src)));
                }
            }
        }
        (compared, bad)
    }

    /// The differential over jinja2's own test templates, the demo tree and the scoping cases
    /// in `scripts/jinja_names.py`.
    #[test]
    fn name_sets_match_jinja2_meta_except_where_rendering_disagrees() {
        let (compared, bad) = differential(include_str!("names_corpus.jsonl"));
        assert!(bad.is_empty(), "{bad:#?}");
        assert!(compared > 80, "the corpus is being reached: {compared}");
    }

    /// The same against the pinned trees' `.j2` files and templated scalars, env-gated in the
    /// T-184 shape because those trees are never committed.
    #[test]
    #[ignore = "corpus gate: JINJA_NAMES_CORPUS=<path> cargo test -p ansible-core --lib names_corpus_gate -- --ignored --nocapture"]
    fn names_corpus_gate() {
        let Ok(path) = std::env::var("JINJA_NAMES_CORPUS") else { return };
        let (compared, bad) = differential(&std::fs::read_to_string(&path).expect("corpus is readable"));
        println!("templates={compared} differed={}", bad.len());
        for b in bad.iter().take(40) {
            println!("  {b}");
        }
        assert!(compared > 0, "an empty corpus measures nothing");
        assert!(bad.is_empty(), "{} rows break the differential", bad.len());
    }

    /// T-212's Symptom: `h` is the loop's, `groups` and `port` are the context's.
    #[test]
    fn a_for_target_is_bound_in_its_body_and_its_iterable_is_read() {
        assert_eq!(names("{% for h in groups.web %}{{ h }}:{{ port }} {% endfor %}"), ["groups", "port"]);
        // Control: the same name outside the loop is a lookup.
        assert_eq!(names("{% for h in xs %}{% endfor %}{{ h }}"), ["xs", "h"]);
        assert_eq!(names("{{ h }}"), ["h"]);
    }

    #[test]
    fn loop_exists_only_inside_a_for_body() {
        assert_eq!(names("{% for h in xs %}{{ loop.index }}{% endfor %}"), ["xs"]);
        assert_eq!(names("{% for h in xs %}{% endfor %}{{ loop.index }}"), ["xs", "loop"]);
        // Not in the loop filter, and not in `else` — both measured to fail at render.
        assert_eq!(names("{% for h in xs if loop %}{% endfor %}"), ["xs", "loop"]);
        assert_eq!(names("{% for h in xs %}{% else %}{{ h }}{{ loop }}{% endfor %}"), ["xs", "h", "loop"]);
        assert_eq!(names("{% for h in xs if h.ok %}{{ h }}{% endfor %}"), ["xs"]);
    }

    #[test]
    fn set_binds_from_the_statement_on_and_a_block_set_after_its_body() {
        assert_eq!(names("{% set q = 1 %}{{ q }}"), Vec::<String>::new());
        assert_eq!(names("{{ q }}{% set q = 1 %}"), ["q"]);
        assert_eq!(names("{% set q = q + 1 %}"), ["q"]);
        assert_eq!(names("{% set a, b = 1, c %}{{ a }}{{ b }}"), ["c"]);
        assert_eq!(names("{% set q %}{{ q }}{{ r }}{% endset %}{{ q }}"), ["q", "r"]);
        assert_eq!(names("{% set ns.x = 1 %}"), ["ns"]);
        assert_eq!(names("{% set ns = namespace() %}{% set ns.x = 1 %}{{ ns.x }}"), ["namespace"]);
    }

    #[test]
    fn a_set_inside_a_loop_does_not_leak_and_one_inside_an_if_does() {
        assert_eq!(names("{% for i in xs %}{% set q = i %}{{ q }}{% endfor %}{{ q }}"), ["xs", "q"]);
        assert_eq!(names("{% if c %}{% set q = 1 %}{{ q }}{% endif %}{{ q }}"), ["c"]);
        // Not visible to a sibling branch, which renders only when the `set` did not run.
        assert_eq!(names("{% if c %}{% set q = 1 %}{% else %}{{ q }}{% endif %}"), ["c", "q"]);
    }

    #[test]
    fn with_binds_for_its_body_and_reads_every_value_outside_it() {
        assert_eq!(names("{% with a = z %}{{ a }}{% endwith %}"), ["z"]);
        assert_eq!(names("{% with a = 1, b = a %}{{ b }}{% endwith %}{{ a }}"), ["a", "a"]);
    }

    #[test]
    fn macro_parameters_stay_inside_the_macro() {
        assert_eq!(
            names("{% macro f(p, q=d) %}{{ p }}{{ q }}{{ r }}{{ varargs }}{{ kwargs }}{{ caller() }}{% endmacro %}{{ f(1) }}{{ p }}"),
            ["d", "r", "p"]
        );
        assert_eq!(names("{% macro f(a, b=a) %}{% endmacro %}"), Vec::<String>::new());
        assert_eq!(names("{% macro f(n) %}{{ f(n - 1) }}{% endmacro %}"), Vec::<String>::new());
    }

    #[test]
    fn call_filter_block_import_and_from_bind_what_they_introduce() {
        assert_eq!(names("{% call(u) f(xs) %}{{ u }}{% endcall %}{{ u }}"), ["f", "xs", "u"]);
        assert_eq!(names("{% filter replace(a, b) %}{{ c }}{% endfilter %}"), ["a", "b", "c"]);
        assert_eq!(names("{% for i in xs %}{% filter upper %}{{ i }}{% endfilter %}{% endfor %}"), ["xs"]);
        assert_eq!(names("{% block b %}{{ x }}{% set q = 1 %}{% endblock %}{{ q }}"), ["x", "q"]);
        assert_eq!(names("{% import 'm' as m %}{{ m.f() }}{{ n }}"), ["n"]);
        assert_eq!(names("{{ m.f() }}{% import 'm' as m %}"), ["m"]);
        assert_eq!(names("{% block b %}{{ super() }}{% endblock %}{{ super() }}"), ["super"]);
        assert_eq!(names("{% from 'm' import a, b as c %}{{ a }}{{ b }}{{ c }}"), ["b"]);
    }

    /// A keyword argument names the callee's parameter, and a filter or test name is not a
    /// variable — the words the old scanner had to list by hand.
    #[test]
    fn only_name_nodes_are_lookups() {
        assert_eq!(names("{{ xs | map(attribute='a') | list }}"), ["xs"]);
        assert_eq!(names("{{ n | int(base=16) }}"), ["n"]);
        assert_eq!(names("{{ x is divisibleby n }}"), ["x", "n"]);
        // A variable spelled like a filter is still a variable.
        assert_eq!(names("{{ first }} {{ count | length }}"), ["first", "count"]);
        let called: Vec<(String, bool)> = template_lookups("{{ lookup('x', p) }}{{ o.m(a) }}")
            .unwrap()
            .into_iter()
            .map(|l| (l.name, l.called))
            .collect();
        assert_eq!(
            called,
            [("lookup".into(), true), ("p".into(), false), ("o".into(), false), ("a".into(), false)]
        );
    }

    #[test]
    fn spans_and_expressions_point_into_the_source() {
        let src = "a {% for h in hs if h.ok %}{{ h }}{{ b | default(c) }}{% endfor %}";
        let got: Vec<(&str, &str)> = template_lookups(src)
            .unwrap()
            .iter()
            .map(|l| (l.span.slice(src), l.expr.slice(src)))
            .collect();
        assert_eq!(got, [("hs", "hs"), ("b", " b | default(c) "), ("c", " b | default(c) ")]);
    }

    #[test]
    fn a_template_that_does_not_render_is_not_read() {
        assert_eq!(template_lookups("{% for x in xs %}"), None);
        assert_eq!(template_lookups("{{ x"), None);
        assert_eq!(template_lookups("{% frobnicate %}"), None);
        // Control: an ordinary one is.
        assert!(template_lookups("{{ x }}").is_some());
    }
}
