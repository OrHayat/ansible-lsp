//! jinja2 3.1.6's `parse_expression` chain, ported.
//!
//! Recursive descent, one method per precedence level, method names upstream's. Precedence is
//! not a table here any more than it is there — it is the call chain:
//!
//! ```text
//! parse_expression → condexpr → or → and → not → compare → math1 → concat → math2 → pow
//!                  → unary → primary,  then postfix and filter_expr on the way back out
//! ```
//!
//! Two details in that chain do the work, and both are easy to get wrong by writing what
//! looks natural instead of what upstream does:
//!
//! - **`parse_postfix` and `parse_filter_expr` are separate, and both sit inside
//!   `parse_unary`** — at the *bottom*. That is why `r.stdout | length > 0` is a comparison
//!   of a filter of an accessor, and never a claim about `r` (T-186).
//! - **`parse_pow` loops rather than recursing**, so `**` is left-associative. Python's is
//!   right-associative: `2**3**2` is 512 in Python and **64** in Jinja. Measured, not read.
//!
//! [`parse`] is the only way in, and it requires the token stream to reach its end — upstream
//! spells that `if not parser.stream.eos: raise TemplateSyntaxError("chunk after expression")`
//! in `Environment.compile_expression`, which is the exact call Ansible's `when:` goes
//! through. Without it `foo bar` parses as `foo` and the rest is silently dropped, which is
//! this module's own failure mode reintroduced by the fix for it.

use super::ast::{Args, BinOp, CmpOp, Const, Expr, ExprKind, UnOp};
use super::lexer::{self, Cause, Error, Kind, Token};
use crate::parse::Span;

/// Parse one Jinja expression.
///
/// The whole input must be one expression. Anything left over is an error rather than a
/// silently shorter answer.
pub fn parse(src: &str) -> Result<Expr, Error> {
    let toks = lexer::tokens(src)?;
    let mut p = Parser { src, toks: &toks, pos: 0, depth: 0 };
    let expr = p.parse_expression()?;
    if p.current().kind != Kind::Eof {
        return Err(Error { cause: Cause::Trailing, ..p.fail_here("chunk after expression") });
    }
    Ok(expr)
}

/// One expression from an existing token stream, plus where it stopped.
///
/// A statement is a tag name, then an expression, then modifiers — `{% include 'a.j2' ignore
/// missing %}` — so its parser has to read *an* expression rather than a whole input. Upstream
/// is the same shape: `parse_include` calls `self.parse_expression()` and then keeps reading
/// the same stream. [`parse`] is that call plus an end-of-stream check.
pub(super) fn expression_at(
    src: &str,
    toks: &[Token],
    pos: usize,
) -> Result<(Expr, usize), Error> {
    let mut p = Parser { src, toks, pos, depth: 0 };
    let expr = p.parse_expression()?;
    Ok((expr, p.pos))
}

/// How deep the recursive descent may go before it refuses.
///
/// Not a style choice — without it, `"("*40 + "1" + ")"*40` **overflows the stack and aborts
/// the process**. Measured: a debug build dies between 35 and 40 nested parentheses, a release
/// build between 100 and 500, because one nesting level costs a dozen frames through the
/// precedence chain. An LSP that segfaults on a pathological file is worse than one that
/// declines to answer, and a `when:` is attacker-supplied in exactly the sense that matters:
/// it comes from a file the editor opened.
///
/// Upstream refuses too — Python's recursion limit turns 100 parentheses into a `RecursionError`
/// — so a ceiling is faithful rather than a divergence. The *height* differs: jinja2 gets to
/// about 99, this stops at 24, which is still far past anything a real expression reaches and
/// leaves margin under the shallowest stack measured (Windows' 1 MiB main thread, debug).
const MAX_DEPTH: u32 = 24;

struct Parser<'a> {
    src: &'a str,
    toks: &'a [Token],
    pos: usize,
    depth: u32,
}

impl<'a> Parser<'a> {
    // ------------------------------------------------------------------ stream

    fn current(&self) -> Token {
        self.toks[self.pos.min(self.toks.len() - 1)]
    }

    /// One token of lookahead — upstream's `stream.look()`. Needed in exactly two places:
    /// `not in` and a keyword argument's `name =`.
    fn look(&self) -> Token {
        self.toks[(self.pos + 1).min(self.toks.len() - 1)]
    }

    /// Advance, saturating at [`Kind::Eof`].
    ///
    /// Nothing calls this at the end — every caller has matched a token first — but the
    /// saturation is not decoration: `span_from` indexes `toks[pos - 1]`, so a `pos` allowed
    /// to run past the end would turn a future editing mistake into a panic instead of a
    /// refusal. Written as a clamp rather than an `if` so it is one path, not two.
    fn next(&mut self) -> Token {
        let t = self.current();
        self.pos = (self.pos + 1).min(self.toks.len() - 1);
        t
    }

    fn skip_if(&mut self, kind: Kind) -> bool {
        if self.current().kind == kind {
            self.next();
            true
        } else {
            false
        }
    }

    /// `stream.current.test("name:if")` — a keyword is a `Name` token with a given spelling,
    /// never its own token kind. Jinja has no reserved words at the lexical level.
    fn at_keyword(&self, word: &str) -> bool {
        let t = self.current();
        t.kind == Kind::Name && t.span.slice(self.src) == word
    }

    fn skip_keyword(&mut self, word: &str) -> bool {
        if self.at_keyword(word) {
            self.next();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, kind: Kind) -> Result<Token, Error> {
        if self.current().kind == kind {
            Ok(self.next())
        } else {
            Err(self.fail_here(&format!("expected {kind:?}, got {:?}", self.current().kind)))
        }
    }

    /// Enter one level of recursion. The matching [`Self::leave`] is bound before `?` can
    /// escape, so the counter is balanced on the error path too.
    fn enter(&mut self) -> Result<(), Error> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(Error { cause: Cause::Depth, ..self.fail_here("expression nests too deeply") });
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    /// `Eof` and `Parse` are the same refusal worded differently, but they are not the same
    /// event: one means the input ran out, the other means a token was wrong. Deriving it
    /// from the cursor rather than from each call site means no site can get it wrong.
    fn fail_here(&self, msg: &str) -> Error {
        let cause = match self.current().kind {
            Kind::Eof => Cause::Eof,
            _ => Cause::Parse,
        };
        Error { msg: msg.to_string(), span: self.current().span, cause }
    }

    /// A node covers from the first token consumed to the last. `pos` has already moved past
    /// the node, so the end comes from the previous token — and when nothing was consumed
    /// (an empty `()`), from the current one.
    fn span_from(&self, start: usize) -> Span {
        let from = self.toks[start.min(self.toks.len() - 1)].span.start;
        let to = if self.pos > start {
            self.toks[self.pos - 1].span.end
        } else {
            self.toks[start.min(self.toks.len() - 1)].span.end
        };
        Span { start: from, end: to }
    }

    fn node(&self, start: usize, kind: ExprKind) -> Expr {
        Expr { kind, span: self.span_from(start) }
    }

    // ------------------------------------------------------------- precedence

    /// Upstream takes `with_condexpr`, which statement forms set to `false` so that the `if`
    /// in `{% if %}` is not eaten as a conditional expression. Nothing in an expression sets
    /// it, so it is not a parameter here; it comes back with T-040.
    fn parse_expression(&mut self) -> Result<Expr, Error> {
        self.enter()?;
        let r = self.parse_condexpr();
        self.leave();
        r
    }

    fn parse_condexpr(&mut self) -> Result<Expr, Error> {
        let start = self.pos;
        let mut expr1 = self.parse_or()?;
        while self.skip_keyword("if") {
            let test = self.parse_or()?;
            // `else` is optional: `{{ 'x' if c }}` renders nothing when `c` is false.
            let or_else =
                if self.skip_keyword("else") { Some(Box::new(self.parse_condexpr()?)) } else { None };
            expr1 = self.node(
                start,
                ExprKind::CondExpr { test: Box::new(test), then: Box::new(expr1), or_else },
            );
        }
        Ok(expr1)
    }

    fn parse_or(&mut self) -> Result<Expr, Error> {
        let start = self.pos;
        let mut left = self.parse_and()?;
        while self.skip_keyword("or") {
            let right = self.parse_and()?;
            left = self.bin(start, BinOp::Or, left, right);
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, Error> {
        let start = self.pos;
        let mut left = self.parse_not()?;
        while self.skip_keyword("and") {
            let right = self.parse_not()?;
            left = self.bin(start, BinOp::And, left, right);
        }
        Ok(left)
    }

    fn parse_not(&mut self) -> Result<Expr, Error> {
        let start = self.pos;
        if self.skip_keyword("not") {
            self.enter()?;
            let node = self.parse_not();
            self.leave();
            let node = node?;
            return Ok(self.node(start, ExprKind::Unary { op: UnOp::Not, node: Box::new(node) }));
        }
        self.parse_compare()
    }

    /// One `Compare` node with a list of operands, not a tree — so `1 < 2 < 3` means what it
    /// says rather than comparing a boolean against `3`.
    fn parse_compare(&mut self) -> Result<Expr, Error> {
        let start = self.pos;
        let expr = self.parse_math1()?;
        let mut ops = Vec::new();
        loop {
            let op = match self.current().kind {
                Kind::Eq => Some(CmpOp::Eq),
                Kind::Ne => Some(CmpOp::Ne),
                Kind::Gt => Some(CmpOp::Gt),
                Kind::Gteq => Some(CmpOp::Gteq),
                Kind::Lt => Some(CmpOp::Lt),
                Kind::Lteq => Some(CmpOp::Lteq),
                _ => None,
            };
            if let Some(op) = op {
                self.next();
                ops.push((op, self.parse_math1()?));
            } else if self.at_keyword("in") {
                self.next();
                ops.push((CmpOp::In, self.parse_math1()?));
            } else if self.at_keyword("not") && {
                let l = self.look();
                l.kind == Kind::Name && l.span.slice(self.src) == "in"
            } {
                self.next();
                self.next();
                ops.push((CmpOp::NotIn, self.parse_math1()?));
            } else {
                break;
            }
        }
        if ops.is_empty() {
            return Ok(expr);
        }
        Ok(self.node(start, ExprKind::Compare { expr: Box::new(expr), ops }))
    }

    fn parse_math1(&mut self) -> Result<Expr, Error> {
        let start = self.pos;
        let mut left = self.parse_concat()?;
        loop {
            let op = match self.current().kind {
                Kind::Add => BinOp::Add,
                Kind::Sub => BinOp::Sub,
                _ => break,
            };
            self.next();
            let right = self.parse_concat()?;
            left = self.bin(start, op, left, right);
        }
        Ok(left)
    }

    /// n-ary: `'a' ~ b ~ 'c'` is one `Concat` of three, not two nested binaries.
    fn parse_concat(&mut self) -> Result<Expr, Error> {
        let start = self.pos;
        let mut args = vec![self.parse_math2()?];
        while self.skip_if(Kind::Tilde) {
            args.push(self.parse_math2()?);
        }
        if args.len() == 1 {
            return Ok(args.pop().expect("just checked"));
        }
        Ok(self.node(start, ExprKind::Concat(args)))
    }

    fn parse_math2(&mut self) -> Result<Expr, Error> {
        let start = self.pos;
        let mut left = self.parse_pow()?;
        loop {
            let op = match self.current().kind {
                Kind::Mul => BinOp::Mul,
                Kind::Div => BinOp::Div,
                Kind::FloorDiv => BinOp::FloorDiv,
                Kind::Mod => BinOp::Mod,
                _ => break,
            };
            self.next();
            let right = self.parse_pow()?;
            left = self.bin(start, op, left, right);
        }
        Ok(left)
    }

    /// A loop, not a recursion — which makes `**` **left**-associative, unlike Python's.
    /// `2**3**2` is 64 here and in Jinja, and 512 in Python. Measured on 3.1.6.
    fn parse_pow(&mut self) -> Result<Expr, Error> {
        let start = self.pos;
        let mut left = self.parse_unary(true)?;
        while self.skip_if(Kind::Pow) {
            let right = self.parse_unary(true)?;
            left = self.bin(start, BinOp::Pow, left, right);
        }
        Ok(left)
    }

    /// The bottom of the chain, and where accessors and filters attach.
    ///
    /// `with_filter` is false on the recursive call so that in `-x|f` the filter applies to
    /// the negation rather than to `x`.
    fn parse_unary(&mut self, with_filter: bool) -> Result<Expr, Error> {
        let start = self.pos;
        let mut node = match self.current().kind {
            Kind::Sub => {
                self.next();
                self.enter()?;
                let n = self.parse_unary(false);
                self.leave();
                self.node(start, ExprKind::Unary { op: UnOp::Neg, node: Box::new(n?) })
            }
            Kind::Add => {
                self.next();
                self.enter()?;
                let n = self.parse_unary(false);
                self.leave();
                self.node(start, ExprKind::Unary { op: UnOp::Pos, node: Box::new(n?) })
            }
            _ => self.parse_primary()?,
        };
        node = self.parse_postfix(node, start)?;
        if with_filter {
            node = self.parse_filter_expr(node, start)?;
        }
        Ok(node)
    }

    fn parse_primary(&mut self) -> Result<Expr, Error> {
        let start = self.pos;
        let token = self.current();
        let kind = match token.kind {
            Kind::Name => {
                self.next();
                match token.span.slice(self.src) {
                    "true" | "True" => ExprKind::Const(Const::Bool(true)),
                    "false" | "False" => ExprKind::Const(Const::Bool(false)),
                    "none" | "None" => ExprKind::Const(Const::None),
                    name => ExprKind::Name(name.to_string()),
                }
            }
            Kind::Str => {
                // Adjacent strings concatenate at parse time, Python-style: `'a' 'b'` is one
                // `Const`, not two nodes and not an implicit `~`.
                let mut buf = lexer::string_value(self.src, token.span)?;
                self.next();
                while self.current().kind == Kind::Str {
                    buf.push_str(&lexer::string_value(self.src, self.current().span)?);
                    self.next();
                }
                ExprKind::Const(Const::Str(buf))
            }
            Kind::Int => {
                self.next();
                ExprKind::Const(Const::Int(lexer::int_value(self.src, token.span)?))
            }
            Kind::Float => {
                self.next();
                ExprKind::Const(Const::Float(lexer::float_value(self.src, token.span)?))
            }
            Kind::Lparen => {
                self.next();
                let node = self.parse_tuple()?;
                self.expect(Kind::Rparen)?;
                // Parentheses are not part of the node upstream: `(1)` is `Const(1)` with the
                // lineno of the `1`. Re-span so ours covers the parentheses, which is what a
                // reader expects the cursor to select.
                return Ok(Expr { kind: node.kind, span: self.span_from(start) });
            }
            Kind::Lbracket => return self.parse_list(),
            Kind::Lbrace => return self.parse_dict(),
            _ => {
                return Err(self.fail_here(&format!("unexpected {:?}", token.kind)));
            }
        };
        Ok(self.node(start, kind))
    }

    // -------------------------------------------------------------- containers

    /// Upstream's `parse_tuple`, cut to the one caller this grammar has: a parenthesised
    /// expression.
    ///
    /// Upstream's five parameters all serve statement forms — `simplified` and
    /// `with_namespace` for assignment targets, `extra_end_rules` for `{% for a, b in %}`,
    /// `with_condexpr` for `{% if %}`. None has a caller here, so none is a parameter here;
    /// they come back with T-040. `explicit_parentheses` in particular is always true, which
    /// is why upstream's "Expected an expression" arm has no counterpart: it fires only for a
    /// tuple with no brackets around it, and nothing in an expression reaches that.
    ///
    /// `is_tuple_end` upstream tests `variable_end`/`block_end`/`rparen`. This state has no
    /// delimiter tokens, so [`Kind::Eof`] stands in for the first two.
    fn parse_tuple(&mut self) -> Result<Expr, Error> {
        let start = self.pos;
        let mut args: Vec<Expr> = Vec::new();
        let mut is_tuple = false;
        loop {
            if !args.is_empty() {
                // Always a comma: the loop only comes back round when the previous iteration
                // saw one, which is what sets `is_tuple` below.
                self.next();
            }
            if matches!(self.current().kind, Kind::Rparen | Kind::Eof) {
                break;
            }
            args.push(self.parse_expression()?);
            if self.current().kind == Kind::Comma {
                is_tuple = true;
            } else {
                break;
            }
        }
        if !is_tuple {
            if let Some(only) = args.pop() {
                return Ok(only);
            }
        }
        Ok(self.node(start, ExprKind::Tuple(args)))
    }

    fn parse_list(&mut self) -> Result<Expr, Error> {
        let start = self.pos;
        self.next(); // the `[`, which `parse_primary` matched before dispatching here
        let mut items = Vec::new();
        loop {
            if self.skip_if(Kind::Rbracket) {
                break;
            }
            if !items.is_empty() {
                self.expect(Kind::Comma)?;
                // A trailing comma is allowed, so re-check before parsing another item.
                if self.skip_if(Kind::Rbracket) {
                    break;
                }
            }
            items.push(self.parse_expression()?);
        }
        Ok(self.node(start, ExprKind::List(items)))
    }

    fn parse_dict(&mut self) -> Result<Expr, Error> {
        let start = self.pos;
        self.next(); // the `{`, matched by `parse_primary`
        let mut items = Vec::new();
        loop {
            if self.skip_if(Kind::Rbrace) {
                break;
            }
            if !items.is_empty() {
                self.expect(Kind::Comma)?;
                if self.skip_if(Kind::Rbrace) {
                    break;
                }
            }
            let key = self.parse_expression()?;
            self.expect(Kind::Colon)?;
            let value = self.parse_expression()?;
            items.push((key, value));
        }
        Ok(self.node(start, ExprKind::Dict(items)))
    }

    // ----------------------------------------------------------------- postfix

    /// Accessors and calls. Deliberately *not* filters — see [`Self::parse_filter_expr`].
    fn parse_postfix(&mut self, mut node: Expr, start: usize) -> Result<Expr, Error> {
        loop {
            match self.current().kind {
                Kind::Dot | Kind::Lbracket => node = self.parse_subscript(node, start)?,
                Kind::Lparen => node = self.parse_call(node, start)?,
                _ => break,
            }
        }
        Ok(node)
    }

    /// Filters and tests, applied *after* all accessors have bound. The split from
    /// [`Self::parse_postfix`] is the reason `r.stdout | length` filters the accessor and not
    /// the root, and it is the single most consequential line in this file.
    fn parse_filter_expr(&mut self, mut node: Expr, start: usize) -> Result<Expr, Error> {
        loop {
            if self.current().kind == Kind::Pipe {
                node = self.parse_filter(node, start)?;
            } else if self.at_keyword("is") {
                node = self.parse_test(node, start)?;
            } else if self.current().kind == Kind::Lparen {
                node = self.parse_call(node, start)?;
            } else {
                break;
            }
        }
        Ok(node)
    }

    /// `.name` is a `Getattr`; everything else — `.0`, `['k']`, `[i]`, a slice — is a
    /// `Getitem` whose argument is an expression. That is what makes `hostvars[h]`
    /// representable at all, and it is why a non-literal subscript stops being a refusal.
    fn parse_subscript(&mut self, node: Expr, start: usize) -> Result<Expr, Error> {
        let token = self.next();
        if token.kind == Kind::Dot {
            let attr = self.current();
            match attr.kind {
                Kind::Name => {
                    self.next();
                    let attr = attr.span.slice(self.src).to_string();
                    return Ok(self.node(start, ExprKind::Getattr { node: Box::new(node), attr }));
                }
                Kind::Int => {
                    self.next();
                    let idx = lexer::int_value(self.src, attr.span)?;
                    let arg = Expr { kind: ExprKind::Const(Const::Int(idx)), span: attr.span };
                    return Ok(self.node(
                        start,
                        ExprKind::Getitem { node: Box::new(node), arg: Box::new(arg) },
                    ));
                }
                _ => return Err(self.fail_here("expected name or number")),
            }
        }
        // `[`. Several comma-separated subscripts become a tuple index, as in numpy.
        let mut args: Vec<Expr> = Vec::new();
        loop {
            if self.skip_if(Kind::Rbracket) {
                break;
            }
            if !args.is_empty() {
                self.expect(Kind::Comma)?;
            }
            args.push(self.parse_subscribed()?);
        }
        let arg = if args.len() == 1 {
            args.pop().expect("just checked")
        } else {
            self.node(start, ExprKind::Tuple(args))
        };
        Ok(self.node(start, ExprKind::Getitem { node: Box::new(node), arg: Box::new(arg) }))
    }

    /// One subscript, which may be a slice. Upstream builds `Slice(start, stop, step)` with
    /// `None` for each part left out.
    fn parse_subscribed(&mut self) -> Result<Expr, Error> {
        let start = self.pos;
        let mut parts: Vec<Option<Expr>> = Vec::new();

        if self.current().kind == Kind::Colon {
            self.next();
            parts.push(None);
        } else {
            let node = self.parse_expression()?;
            if self.current().kind != Kind::Colon {
                return Ok(node);
            }
            self.next();
            parts.push(Some(node));
        }

        if self.current().kind == Kind::Colon {
            parts.push(None);
        } else if !matches!(self.current().kind, Kind::Rbracket | Kind::Comma) {
            parts.push(Some(self.parse_expression()?));
        } else {
            parts.push(None);
        }

        if self.current().kind == Kind::Colon {
            self.next();
            if !matches!(self.current().kind, Kind::Rbracket | Kind::Comma) {
                parts.push(Some(self.parse_expression()?));
            } else {
                parts.push(None);
            }
        } else {
            parts.push(None);
        }

        let mut it = parts.into_iter();
        let (a, b, c) = (it.next().flatten(), it.next().flatten(), it.next().flatten());
        Ok(self.node(
            start,
            ExprKind::Slice {
                start: a.map(Box::new),
                stop: b.map(Box::new),
                step: c.map(Box::new),
            },
        ))
    }

    // -------------------------------------------------------- calls and filters

    /// `parse_call_args`. The ordering constraints are upstream's `ensure()` calls: no
    /// positional after `*`, no `*` after `**`, no positional after a keyword.
    fn parse_call_args(&mut self) -> Result<Args, Error> {
        self.next(); // the `(`, checked by every caller before dispatching here
        let mut out = Args::default();
        let mut require_comma = false;

        loop {
            if self.skip_if(Kind::Rparen) {
                break;
            }
            if require_comma {
                self.expect(Kind::Comma)?;
                if self.skip_if(Kind::Rparen) {
                    break; // trailing comma
                }
            }
            match self.current().kind {
                Kind::Mul => {
                    if out.dyn_args.is_some() || out.dyn_kwargs.is_some() {
                        return Err(self.fail_here("invalid syntax for function call expression"));
                    }
                    self.next();
                    out.dyn_args = Some(Box::new(self.parse_expression()?));
                }
                Kind::Pow => {
                    if out.dyn_kwargs.is_some() {
                        return Err(self.fail_here("invalid syntax for function call expression"));
                    }
                    self.next();
                    out.dyn_kwargs = Some(Box::new(self.parse_expression()?));
                }
                _ => {
                    if self.current().kind == Kind::Name && self.look().kind == Kind::Assign {
                        if out.dyn_kwargs.is_some() {
                            return Err(
                                self.fail_here("invalid syntax for function call expression")
                            );
                        }
                        let key = self.current().span.slice(self.src).to_string();
                        self.next();
                        self.next();
                        out.kwargs.push((key, self.parse_expression()?));
                    } else {
                        if out.dyn_args.is_some()
                            || out.dyn_kwargs.is_some()
                            || !out.kwargs.is_empty()
                        {
                            return Err(
                                self.fail_here("invalid syntax for function call expression")
                            );
                        }
                        out.args.push(self.parse_expression()?);
                    }
                }
            }
            require_comma = true;
        }
        Ok(out)
    }

    fn parse_call(&mut self, node: Expr, start: usize) -> Result<Expr, Error> {
        let args = self.parse_call_args()?;
        Ok(self.node(start, ExprKind::Call { node: Box::new(node), args }))
    }

    /// A filter name may be dotted — `ansible.builtin.length` — so the dots are joined into
    /// the name rather than parsed as accessors.
    fn parse_filter(&mut self, mut node: Expr, start: usize) -> Result<Expr, Error> {
        while self.current().kind == Kind::Pipe {
            self.next();
            let mut name = self.expect(Kind::Name)?.span.slice(self.src).to_string();
            while self.current().kind == Kind::Dot {
                self.next();
                name.push('.');
                name.push_str(self.expect(Kind::Name)?.span.slice(self.src));
            }
            let args =
                if self.current().kind == Kind::Lparen { self.parse_call_args()? } else { Args::default() };
            node = self.node(start, ExprKind::Filter { node: Box::new(node), name, args });
        }
        Ok(node)
    }

    /// `is defined`, `is not defined`, `is divisibleby 3`.
    ///
    /// A negated test is `Not(Test(..))` — upstream reads the `not` itself and wraps, so
    /// there is no "negated" flag on the node.
    fn parse_test(&mut self, node: Expr, start: usize) -> Result<Expr, Error> {
        self.next(); // `is`
        let negated = self.skip_keyword("not");
        let mut name = self.expect(Kind::Name)?.span.slice(self.src).to_string();
        while self.current().kind == Kind::Dot {
            self.next();
            name.push('.');
            name.push_str(self.expect(Kind::Name)?.span.slice(self.src));
        }

        let mut args = Args::default();
        if self.current().kind == Kind::Lparen {
            args = self.parse_call_args()?;
        } else if matches!(
            self.current().kind,
            Kind::Name | Kind::Str | Kind::Int | Kind::Float | Kind::Lparen | Kind::Lbracket | Kind::Lbrace
        ) && !(self.at_keyword("else") || self.at_keyword("or") || self.at_keyword("and"))
        {
            if self.at_keyword("is") {
                return Err(self.fail_here("You cannot chain multiple tests with is"));
            }
            // A bare test argument gets accessors but no filters — `is sameas foo.bar` works,
            // `is sameas foo|x` does not bind the filter to the argument.
            let arg_start = self.pos;
            let arg = self.parse_primary()?;
            let arg = self.parse_postfix(arg, arg_start)?;
            args.args.push(arg);
        }

        let test = self.node(start, ExprKind::Test { node: Box::new(node), name, args });
        if negated {
            return Ok(self.node(start, ExprKind::Unary { op: UnOp::Not, node: Box::new(test) }));
        }
        Ok(test)
    }

    fn bin(&self, start: usize, op: BinOp, left: Expr, right: Expr) -> Expr {
        self.node(start, ExprKind::Bin { op, left: Box::new(left), right: Box::new(right) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    /// The same walk `scripts/jinja_ast.py` does over `nodes.Node.fields`, so the two sides
    /// produce comparable JSON. Spans are excluded — upstream has none to compare against,
    /// which is why they get their own tests below.
    fn dump(e: &Expr) -> Value {
        fn args_fields(a: &Args) -> (Vec<Value>, Vec<Value>, Value, Value) {
            let kwargs: Vec<Value> = a
                .kwargs
                .iter()
                .map(|(k, v)| json!({"t": "Keyword", "key": k, "value": dump(v)}))
                .collect();
            (
                a.args.iter().map(dump).collect::<Vec<_>>(),
                kwargs,
                a.dyn_args.as_ref().map(|d| dump(d)).unwrap_or(Value::Null),
                a.dyn_kwargs.as_ref().map(|d| dump(d)).unwrap_or(Value::Null),
            )
        }
        match &e.kind {
            ExprKind::Const(c) => {
                let v = match c {
                    Const::Str(s) => json!(s),
                    Const::Int(i) => json!(i),
                    Const::Float(f) => json!(f),
                    Const::Bool(b) => json!(b),
                    Const::None => Value::Null,
                };
                json!({"t": "Const", "value": v})
            }
            ExprKind::Name(n) => json!({"t": "Name", "name": n}),
            ExprKind::List(items) => {
                json!({"t": "List", "items": items.iter().map(dump).collect::<Vec<_>>()})
            }
            ExprKind::Tuple(items) => {
                json!({"t": "Tuple", "items": items.iter().map(dump).collect::<Vec<_>>()})
            }
            ExprKind::Dict(items) => json!({
                "t": "Dict",
                "items": items.iter()
                    .map(|(k, v)| json!({"t": "Pair", "key": dump(k), "value": dump(v)}))
                    .collect::<Vec<_>>()
            }),
            ExprKind::Getattr { node, attr } => {
                json!({"t": "Getattr", "node": dump(node), "attr": attr})
            }
            ExprKind::Getitem { node, arg } => {
                json!({"t": "Getitem", "node": dump(node), "arg": dump(arg)})
            }
            ExprKind::Slice { start, stop, step } => json!({
                "t": "Slice",
                "start": start.as_ref().map(|x| dump(x)).unwrap_or(Value::Null),
                "stop": stop.as_ref().map(|x| dump(x)).unwrap_or(Value::Null),
                "step": step.as_ref().map(|x| dump(x)).unwrap_or(Value::Null),
            }),
            ExprKind::Filter { node, name, args } => {
                let (a, kw, da, dk) = args_fields(args);
                json!({"t": "Filter", "node": dump(node), "name": name,
                       "args": a, "kwargs": kw, "dyn_args": da, "dyn_kwargs": dk})
            }
            ExprKind::Test { node, name, args } => {
                let (a, kw, da, dk) = args_fields(args);
                json!({"t": "Test", "node": dump(node), "name": name,
                       "args": a, "kwargs": kw, "dyn_args": da, "dyn_kwargs": dk})
            }
            ExprKind::Call { node, args } => {
                let (a, kw, da, dk) = args_fields(args);
                json!({"t": "Call", "node": dump(node),
                       "args": a, "kwargs": kw, "dyn_args": da, "dyn_kwargs": dk})
            }
            ExprKind::Compare { expr, ops } => json!({
                "t": "Compare",
                "expr": dump(expr),
                "ops": ops.iter()
                    .map(|(op, e)| json!({"t": "Operand", "op": op.op_name(), "expr": dump(e)}))
                    .collect::<Vec<_>>()
            }),
            ExprKind::Concat(nodes) => {
                json!({"t": "Concat", "nodes": nodes.iter().map(dump).collect::<Vec<_>>()})
            }
            ExprKind::CondExpr { test, then, or_else } => json!({
                "t": "CondExpr",
                "test": dump(test),
                "expr1": dump(then),
                "expr2": or_else.as_ref().map(|x| dump(x)).unwrap_or(Value::Null),
            }),
            ExprKind::Bin { op, left, right } => {
                json!({"t": op.node_name(), "left": dump(left), "right": dump(right)})
            }
            ExprKind::Unary { op, node } => json!({"t": op.node_name(), "node": dump(node)}),
        }
    }

    fn tree(src: &str) -> Value {
        dump(&parse(src).unwrap_or_else(|e| panic!("{src:?} must parse: {}", e.msg)))
    }

    // ---------------------------------------------------------------- differential

    /// Every tree this parser builds against every tree jinja2 3.1.6 builds, over 1126
    /// expressions — jinja2's own test suite, this repo's `demo/`, and a hand-written
    /// adversarial block. Regenerate with:
    ///
    /// ```text
    /// python scripts/jinja_ast.py <jinja2-sdist>/tests demo \
    ///     > crates/ansible-core/src/jinja/parser_corpus.jsonl
    /// ```
    ///
    /// Most of the refusal rows are statement bodies harvested from `{% %}`, which are
    /// correctly not expressions. Both sides must refuse the same inputs, so the bad paths
    /// are differential too rather than merely exercised.
    ///
    /// Refusals are compared on *stage*, not on message text. Upstream's wording is Python
    /// prose this port does not reproduce, but which stage gave up — the tokeniser, the parser
    /// on a token, the parser at end of input, or the trailing-input check — is a behavioural
    /// claim, and 472 of 476 refusals agree on it exactly.
    /// Inputs jinja2 accepts and this parser refuses, each with the reason. Refusing where
    /// upstream answers is legal only when it is *declared*: a refusal not on this list fails
    /// the differential, and the list's length is asserted so it cannot grow unnoticed into
    /// "we don't parse much any more".
    const KNOWN_REFUSALS: &[(&str, &str)] = &[
        (
            "\"\\N{HOT SPRINGS}\"",
            "\\N{...} resolves through the Unicode name database, which Rust does not ship",
        ),
        // Python integers are unbounded and `Const::Int` is an `i64`. Refusing beats
        // wrapping: a wrapped literal would be presented as the number the author wrote.
        ("99999999999999999999999999", "integer literal wider than i64"),
        ("x[99999999999999999999999999]", "integer literal wider than i64"),
        ("x.99999999999999999999999999", "integer literal wider than i64"),
    ];

    #[test]
    fn the_whole_corpus_parses_exactly_as_jinja2_does() {
        let corpus = include_str!("parser_corpus.jsonl");
        let (mut ok, mut refused) = (0, 0);
        let mut stage_diffs: Vec<(String, String, &str)> = Vec::new();
        let mut declared = 0;

        for line in corpus.lines().filter(|l| !l.trim().is_empty()) {
            let row: Value = serde_json::from_str(line).expect("corpus line parses");
            let src = row["src"].as_str().expect("every row has a source");
            match (parse(src), row.get("ast")) {
                (Ok(got), Some(want)) => {
                    assert_eq!(&dump(&got), want, "{src:?}");
                    ok += 1;
                }
                (Err(e), Some(_)) => match KNOWN_REFUSALS.iter().find(|(s, _)| *s == src) {
                    Some(_) => declared += 1,
                    None => panic!("{src:?} must parse, refused with {:?}", e.msg),
                },
                (Ok(got), None) => {
                    panic!("{src:?} must be refused, but produced {:?}", dump(&got))
                }
                (Err(e), None) => {
                    let want = row["cause"].as_str().expect("every refusal names a stage");
                    let got = match e.cause {
                        Cause::Lex => "lex",
                        Cause::Eof => "eof",
                        Cause::Parse => "parse",
                        Cause::Trailing => "trailing",
                        Cause::Depth => "depth",
                    };
                    if got != want {
                        stage_diffs.push((src.to_string(), want.to_string(), got));
                    }
                    refused += 1;
                }
            }
        }
        // Eager tokenising can only move a refusal *earlier* — into the lexer — because the
        // whole input is scanned before the parser runs. jinja2 pulls tokens lazily and stops
        // as soon as the parser is satisfied, so a lexical fault after that point is one it
        // never reaches. Both refuse either way; only the stage moves, and it can only move
        // one direction. A disagreement in any other direction is a real divergence.
        for (src, want, got) in &stage_diffs {
            assert_eq!(
                *got, "lex",
                "{src:?}: jinja2 said {want}, we said {got} — not explained by eager lexing"
            );
        }
        assert_eq!(
            stage_diffs.len(),
            4,
            "{} inputs refuse at a different stage, not 4 — the eager/lazy set moved",
            stage_diffs.len()
        );
        assert!(ok > 600, "only {ok} trees compared — corpus regenerated wrong?");
        assert!(refused > 400, "only {refused} refusals — the bad paths left the corpus");
        // Every declared divergence must actually occur, or it is stale and hiding nothing.
        assert_eq!(
            declared,
            KNOWN_REFUSALS.len(),
            "{declared} declared refusals fired of {} listed",
            KNOWN_REFUSALS.len()
        );
    }


    /// The same comparison as the checked-in corpus, against a file of randomly generated
    /// expressions. Env-gated and `#[ignore]`d in the T-184 shape, because the input is
    /// thousands of rows that would triple the repo and differ by seed.
    ///
    /// ```text
    /// PYTHONPATH=scripts python scripts/jinja_fuzz.py 4000 6 > $SCRATCH/fuzz.jsonl
    /// JINJA_FUZZ_CORPUS=$SCRATCH/fuzz.jsonl \
    ///     cargo test -p ansible-core --lib fuzz_corpus -- --ignored --nocapture
    /// ```
    ///
    /// A curated corpus only holds what someone thought to write down; this is what catches
    /// the spellings nobody imagined. Anything that mismatches here should be copied into
    /// `jinja_ast.py`'s adversarial block so it becomes a permanent, checked-in case rather
    /// than something that only fails under one seed.
    ///
    /// **A clean run is also what a broken harness looks like**, so it prints the split and
    /// refuses to pass on a file that is empty or all-refusals.
    #[test]
    #[ignore = "fuzz gate: JINJA_FUZZ_CORPUS=<path> cargo test -p ansible-core --lib fuzz_corpus -- --ignored --nocapture"]
    fn fuzz_corpus() {
        let Ok(path) = std::env::var("JINJA_FUZZ_CORPUS") else { return };
        let text = std::fs::read_to_string(&path).expect("fuzz corpus is readable");
        let (mut ok, mut refused, mut declared) = (0, 0, 0);

        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let row: Value = serde_json::from_str(line).expect("corpus line parses");
            let src = row["src"].as_str().expect("every row has a source");
            match (parse(src), row.get("ast")) {
                (Ok(got), Some(want)) => {
                    assert_eq!(&dump(&got), want, "{src:?}");
                    ok += 1;
                }
                (Err(e), Some(_)) => {
                    // The depth ceiling is a deliberate divergence with its own tests; the
                    // generator caps nesting below it, so seeing one here means the cap
                    // slipped, not that the parser is wrong.
                    assert_ne!(
                        e.msg, "expression nests too deeply",
                        "{src:?} hit the depth ceiling — the generator's cap is too high"
                    );
                    match KNOWN_REFUSALS.iter().find(|(s, _)| *s == src) {
                        Some(_) => declared += 1,
                        None => panic!("{src:?} must parse, refused with {:?}", e.msg),
                    }
                }
                (Ok(got), None) => {
                    panic!("{src:?} must be refused, but produced {:?}", dump(&got))
                }
                (Err(_), None) => refused += 1,
            }
        }
        println!("fuzz: {ok} matched, {refused} refused, {declared} declared divergences");
        assert!(ok > 0 && refused > 0, "a one-sided corpus cannot tell a bug from a bug");
    }

    // ---------------------------------------------------------------- the split

    /// The single most consequential line in the module, asserted directly rather than left
    /// to the corpus: `parse_postfix` binds accessors, `parse_filter_expr` binds filters, and
    /// filters bind *later*. T-186 is the bug where this was not true.
    #[test]
    fn a_filter_applies_to_the_whole_accessor_path_not_to_its_root() {
        assert_eq!(
            tree("r.stdout | length > 0"),
            json!({"t": "Compare",
                   "expr": {"t": "Filter", "name": "length", "args": [], "kwargs": [],
                            "dyn_args": null, "dyn_kwargs": null,
                            "node": {"t": "Getattr", "attr": "stdout",
                                     "node": {"t": "Name", "name": "r"}}},
                   "ops": [{"t": "Operand", "op": "gt", "expr": {"t": "Const", "value": 0}}]})
        );
    }

    /// The same expression written four ways is the same tree. T-187 dissolves here rather
    /// than being fixed: whitespace never reaches the parser.
    #[test]
    fn spelling_does_not_change_the_tree() {
        let want = tree("hosts | length > 0");
        for src in ["hosts|length>0", "hosts  |  length  >  0", "hosts\n|\tlength\n> 0"] {
            assert_eq!(tree(src), want, "{src:?}");
        }
    }

    /// Left-associative, unlike Python. `2**3**2` is 64 in Jinja and 512 in Python, because
    /// `parse_pow` loops instead of recursing. Measured on jinja2 3.1.6.
    #[test]
    fn exponentiation_is_left_associative_unlike_python() {
        assert_eq!(
            tree("2**3**2"),
            json!({"t": "Pow",
                   "left": {"t": "Pow", "left": {"t": "Const", "value": 2},
                            "right": {"t": "Const", "value": 3}},
                   "right": {"t": "Const", "value": 2}})
        );
    }

    /// A chain is one node with two operands, so `1 < 2 < 3` means what it reads as rather
    /// than comparing a boolean with `3`.
    #[test]
    fn comparisons_chain_into_one_node() {
        let t = tree("1 < 2 < 3");
        assert_eq!(t["t"], "Compare");
        assert_eq!(t["ops"].as_array().expect("ops").len(), 2);
    }

    /// `is not defined` is `Not(Test(..))`, with no negation flag anywhere — upstream reads
    /// the `not` in `parse_test` and wraps.
    #[test]
    fn a_negated_test_is_a_not_around_the_test() {
        assert_eq!(
            tree("x is not defined"),
            json!({"t": "Not",
                   "node": {"t": "Test", "name": "defined", "args": [], "kwargs": [],
                            "dyn_args": null, "dyn_kwargs": null,
                            "node": {"t": "Name", "name": "x"}}})
        );
    }

    /// Accessors normalise to two nodes: `.name` is `Getattr`, everything else is `Getitem`
    /// with an expression argument — which is what makes a non-literal subscript
    /// representable instead of refused.
    #[test]
    fn accessors_normalise_to_getattr_and_getitem() {
        assert_eq!(tree("a.0"), tree("a[0]"));
        assert_eq!(
            tree("hostvars[h].x"),
            json!({"t": "Getattr", "attr": "x",
                   "node": {"t": "Getitem", "node": {"t": "Name", "name": "hostvars"},
                            "arg": {"t": "Name", "name": "h"}}})
        );
    }

    /// `true`/`false`/`none` are `Name` tokens the lexer cannot distinguish; it is
    /// `parse_primary` that makes them constants, and both spellings of each work.
    #[test]
    fn keyword_literals_become_constants_in_the_parser() {
        for (src, want) in [
            ("true", json!(true)),
            ("True", json!(true)),
            ("false", json!(false)),
            ("False", json!(false)),
            ("none", Value::Null),
            ("None", Value::Null),
        ] {
            assert_eq!(tree(src), json!({"t": "Const", "value": want}), "{src}");
        }
        // ...and an ordinary name is not touched.
        assert_eq!(tree("truthy"), json!({"t": "Name", "name": "truthy"}));
    }

    #[test]
    fn adjacent_strings_fold_into_one_constant() {
        assert_eq!(tree("'a' 'b'"), json!({"t": "Const", "value": "ab"}));
    }


    // ------------------------------------------------- upstream's own AST assertions

    /// jinja2's `TestSyntax::test_neg_filter_priority`, ported verbatim — the one place their
    /// suite asserts tree *shape* rather than rendered output:
    ///
    /// ```python
    /// node = env.parse("{{ -1|foo }}")
    /// assert isinstance(node.body[0].nodes[0], nodes.Filter)
    /// assert isinstance(node.body[0].nodes[0].node, nodes.Neg)
    /// ```
    ///
    /// This is what `with_filter: false` on the recursive `parse_unary` call buys: the filter
    /// binds to the negation, not to the `1` inside it.
    #[test]
    fn a_filter_after_a_negation_applies_to_the_negation() {
        assert_eq!(
            tree("-1|foo"),
            json!({"t": "Filter", "name": "foo", "args": [], "kwargs": [],
                   "dyn_args": null, "dyn_kwargs": null,
                   "node": {"t": "Neg", "node": {"t": "Const", "value": 1}}})
        );
    }

    /// jinja2's `TestSyntax::test_parse_unary`. Upstream asserts it by rendering —
    /// `{{ -foo["bar"]|abs }}` with `foo={"bar": 42}` gives `42`, not `-42`, which is only
    /// possible if `abs` wraps the negation. Asserted here on the tree, since we do not render.
    #[test]
    fn a_filter_after_a_negated_subscript_wraps_the_whole_negation() {
        assert_eq!(
            tree("-foo[\"bar\"]|abs"),
            json!({"t": "Filter", "name": "abs", "args": [], "kwargs": [],
                   "dyn_args": null, "dyn_kwargs": null,
                   "node": {"t": "Neg", "node": {"t": "Getitem",
                            "node": {"t": "Name", "name": "foo"},
                            "arg": {"t": "Const", "value": "bar"}}}})
        );
        // ...and without the filter the negation is still the outer node.
        assert_eq!(tree("-foo[\"bar\"]")["t"], "Neg");
    }

    /// jinja2's `TestSyntax::test_const`. Upstream renders `{{ none is defined }}` to `True`,
    /// which only works because `none` reached `parse_primary` and became a constant before
    /// the test was applied to it — an undefined *name* would render `False`, as `missing`
    /// does in the same assertion.
    #[test]
    fn a_test_applied_to_a_keyword_literal_sees_a_constant() {
        assert_eq!(
            tree("none is defined")["node"],
            json!({"t": "Const", "value": null})
        );
        assert_eq!(
            tree("missing is defined")["node"],
            json!({"t": "Name", "name": "missing"})
        );
    }

    /// Upstream's `TestParser::test_error_messages` is not portable yet: every one of its six
    /// cases is a statement (`{% for %}`, `{% if %}`, `{% block %}`, an unknown tag), so it
    /// lands with T-040 rather than here. Recorded so the gap is deliberate rather than
    /// forgotten — the expression half of that file is the corpus above.
    #[test]
    fn upstreams_error_message_suite_is_statement_level_and_belongs_to_t_040() {
        // A tag *body* is not an expression, and fails here for the mundane reason that a
        // second name follows the first.
        for body in ["for item in seq", "if foo", "block foo"] {
            assert_eq!(parse(body).expect_err("not an expression").msg, "chunk after expression");
        }
        // But a bare tag *name* is a perfectly good expression — it is a variable read. This
        // is the half of `test_error_messages` that cannot move here even in principle:
        // "unknown tag" is a judgement only the document grammar can make.
        assert_eq!(tree("unknown_tag"), json!({"t": "Name", "name": "unknown_tag"}));
    }

    // ------------------------------------------------------------------ recursion

    /// Without a ceiling this aborts the process rather than returning an error: measured, a
    /// debug build overflows its stack between 35 and 40 nested parentheses and a release
    /// build between 100 and 500. A crash is the one failure mode an LSP cannot recover from,
    /// and `when:` values arrive from files the editor opened.
    ///
    /// The guard has to sit on every recursive shape, not just parentheses — each of these
    /// re-enters the chain by a different route.
    #[test]
    fn deep_nesting_is_refused_rather_than_overflowing_the_stack() {
        let deep = 50_000;
        let cases = [
            ("parens", format!("{}1{}", "(".repeat(deep), ")".repeat(deep))),
            ("not", format!("{}x", "not ".repeat(deep))),
            ("neg", format!("{}x", "-".repeat(deep))),
            ("pos", format!("{}x", "+".repeat(deep))),
            ("list", format!("{}1{}", "[".repeat(deep), "]".repeat(deep))),
            ("dict", format!("{}1{}", "{'a':".repeat(deep), "}".repeat(deep))),
            ("call", format!("f{}1{}", "(".repeat(deep), ")".repeat(deep))),
            ("subscript", format!("x{}1{}", "[".repeat(deep), "]".repeat(deep))),
            ("filter args", format!("x|d{}1{}", "(".repeat(deep), ")".repeat(deep))),
        ];
        for (label, src) in cases {
            let e = parse(&src).expect_err("must be refused");
            assert_eq!(e.msg, "expression nests too deeply", "{label}");
            // Ours alone, so it must never be reported as one of the stages the corpus
            // compares — a ceiling hit is not a claim about what jinja2 does with this input.
            assert_eq!(e.cause, Cause::Depth, "{label}");
        }
    }

    /// The limit has to be *reachable*, or the guard is really a much lower one by accident.
    /// 23 nested parentheses parse; 24 do not. Anything a real condition writes is under 5.
    #[test]
    fn the_depth_ceiling_is_where_it_says_it_is() {
        let ok = format!("{}1{}", "(".repeat(23), ")".repeat(23));
        assert!(parse(&ok).is_ok(), "one below the ceiling must parse");
        let over = format!("{}1{}", "(".repeat(24), ")".repeat(24));
        assert!(parse(&over).is_err(), "the ceiling must actually stop something");
    }

    /// The counter is decremented on the way back out, so breadth is not depth: a long flat
    /// expression must not accumulate its way into a refusal.
    #[test]
    fn depth_is_not_charged_for_breadth() {
        let wide: Vec<String> = (0..2_000).map(|i| i.to_string()).collect();
        let src = format!("[{}]", wide.join(", "));
        assert!(parse(&src).is_ok(), "2000 flat list items is not deep nesting");
        let chained = format!("x{}", " | d".repeat(2_000));
        assert!(parse(&chained).is_ok(), "2000 chained filters is not deep nesting");
        let added = format!("1{}", " + 1".repeat(2_000));
        assert!(parse(&added).is_ok(), "2000 additions is not deep nesting");
    }

    // ---------------------------------------------------------------- refusals

    /// Ansible's own check. `compile_expression` ends with `if not parser.stream.eos: raise
    /// TemplateSyntaxError("chunk after expression")`, and without it each of these parses as
    /// its first token with the rest dropped — a confident answer about `foo`.
    #[test]
    fn anything_left_over_is_refused_rather_than_silently_dropped() {
        for src in ["foo bar", "1, 2", "a b c", "x | length y"] {
            let e = parse(src).expect_err("must not parse");
            assert_eq!(e.msg, "chunk after expression", "{src:?}");
        }
        // The control: each of these is a prefix that *does* parse on its own, so the refusal
        // is about the leftovers and not about the prefix being unreadable.
        for src in ["foo", "1", "a", "x | length"] {
            assert!(parse(src).is_ok(), "{src:?}");
        }
    }

    #[test]
    fn incomplete_expressions_are_refused() {
        for src in ["x ==", "not", "foo.", "x is", "(1", "[1", "{'a':", "f(a=1, 2)"] {
            assert!(parse(src).is_err(), "{src:?} must not parse");
        }
    }

    /// A refusal carries the range to underline, which is what T-040's diagnostic needs and
    /// what `classify` throws away on its way to `Unknown`.
    #[test]
    fn a_refusal_points_at_the_offending_token() {
        let e = parse("foo bar").expect_err("must not parse");
        assert_eq!(e.span.slice("foo bar"), "bar");
        let e = parse("x @ y").expect_err("must not parse");
        assert_eq!(e.span.slice("x @ y"), "@");
    }

    // ------------------------------------------------- paths `parse` cannot reach

    /// `parse_primary` decodes a `Float` token's text, and every `Float` this lexer emits
    /// parses — so the error arm is unreachable through [`parse`]. It is not unreachable
    /// through the type: `Parser` reads whatever token stream it is handed, and a decoder
    /// that panicked on a mismatched span would be a crash waiting for the first edit that
    /// gets the lexer and the parser out of step. Driven directly, the way `lexer.rs` drives
    /// its own decoders.
    #[test]
    fn a_float_token_whose_text_is_not_a_float_is_refused_not_panicked() {
        let src = "abc";
        let toks = [
            Token { kind: Kind::Float, span: Span { start: 0, end: 3 } },
            Token { kind: Kind::Eof, span: Span { start: 3, end: 3 } },
        ];
        let mut p = Parser { src, toks: &toks, pos: 0, depth: 0 };
        let e = p.parse_primary().expect_err("a bogus float span must be refused");
        assert_eq!(e.msg, "invalid float literal: abc");
    }

    // ---------------------------------------------------------------- spans

    /// Upstream keeps `lineno` and nothing else, so the corpus cannot check any of this.
    #[test]
    fn every_node_spans_the_text_it_was_built_from() {
        let src = "r.results[0].stdout | length > 0";
        let e = parse(src).expect("parses");
        assert_eq!(e.span.slice(src), src);
        let ExprKind::Compare { expr, ops } = &e.kind else { panic!("expected a comparison") };
        assert_eq!(expr.span.slice(src), "r.results[0].stdout | length");
        assert_eq!(ops[0].1.span.slice(src), "0");
        let ExprKind::Filter { node, .. } = &expr.kind else { panic!("expected a filter") };
        assert_eq!(node.span.slice(src), "r.results[0].stdout");
        let ExprKind::Getattr { node, .. } = &node.kind else { panic!("expected an accessor") };
        assert_eq!(node.span.slice(src), "r.results[0]");
    }

    /// Parentheses are not a node upstream, so `(1)` is `Const(1)` — but the *span* has to
    /// cover them, or selecting the expression under the cursor drops the brackets.
    #[test]
    fn a_parenthesised_expression_spans_its_parentheses() {
        let src = "(1 + 2)";
        let e = parse(src).expect("parses");
        assert_eq!(e.span.slice(src), "(1 + 2)");
    }

    #[test]
    fn spans_are_byte_ranges_past_non_ascii() {
        let src = "'♨' ~ tail";
        let e = parse(src).expect("parses");
        let ExprKind::Concat(parts) = &e.kind else { panic!("expected a concat") };
        assert_eq!(parts[1].span.slice(src), "tail");
    }

    // ---------------------------------------------------------------- root_name

    /// What T-186 needed and a `String` could not express: the name a lookup resolves, which
    /// is never the whole path.
    #[test]
    fn the_root_of_an_accessor_path_is_the_name_at_its_base() {
        for (src, want) in [
            ("r", Some("r")),
            ("r.stdout", Some("r")),
            ("r['stdout']", Some("r")),
            ("r.results[0].stdout", Some("r")),
            // The subject is `hostvars`; `h` is an argument to it, not on the spine.
            ("hostvars[h].x", Some("hostvars")),
            // A filter is not an accessor, so the chain stops being a plain reference.
            ("r.stdout | length", None),
            ("'literal'", None),
            ("a + b", None),
        ] {
            assert_eq!(parse(src).expect("parses").root_name(), want, "{src}");
        }
    }
}
