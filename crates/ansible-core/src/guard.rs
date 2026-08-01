//! Propositional reasoning over `when:` guards — enough to tell whether the condition on a
//! *use* of a variable is covered by the conditions on its *definitions*.
//!
//! No runtime values are needed: each distinct `when:` sub-expression is treated as an opaque
//! boolean atom, and we ask whether `use-guard ⟹ (def₁ ∨ def₂ ∨ …)` is valid. If it isn't,
//! there's an assignment where the use runs but no definition covered it — a coverage gap.
//! This is decidable (SAT over the atoms). What it can't do — evaluate the atoms, or relate
//! two conditions that share no structure — it deliberately stays silent about.

use std::collections::BTreeSet;

#[derive(Debug, Clone)]
enum Formula {
    True,
    Atom(String),
    Not(Box<Formula>),
    And(Vec<Formula>),
    Or(Vec<Formula>),
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    And,
    Or,
    Not,
    LParen,
    RParen,
    Atom(String),
}

/// Split into words, keeping quoted strings intact and parens as their own words.
fn lex(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let flush = |cur: &mut String, out: &mut Vec<String>| {
        let t = cur.trim();
        if !t.is_empty() {
            out.push(t.to_string());
        }
        cur.clear();
    };
    for c in s.chars() {
        match quote {
            Some(q) => {
                cur.push(c);
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    cur.push(c);
                }
                '(' | ')' => {
                    flush(&mut cur, &mut out);
                    out.push(c.to_string());
                }
                c if c.is_whitespace() => flush(&mut cur, &mut out),
                _ => cur.push(c),
            },
        }
    }
    flush(&mut cur, &mut out);
    out
}

fn tokenize(s: &str) -> Vec<Tok> {
    let mut toks = Vec::new();
    let mut buf: Vec<String> = Vec::new();
    fn flush(buf: &mut Vec<String>, toks: &mut Vec<Tok>) {
        if !buf.is_empty() {
            toks.push(Tok::Atom(buf.join(" ")));
            buf.clear();
        }
    }
    for w in lex(s) {
        match w.as_str() {
            "and" => {
                flush(&mut buf, &mut toks);
                toks.push(Tok::And);
            }
            "or" => {
                flush(&mut buf, &mut toks);
                toks.push(Tok::Or);
            }
            "not" => {
                flush(&mut buf, &mut toks);
                toks.push(Tok::Not);
            }
            "(" => {
                flush(&mut buf, &mut toks);
                toks.push(Tok::LParen);
            }
            ")" => {
                flush(&mut buf, &mut toks);
                toks.push(Tok::RParen);
            }
            _ => buf.push(w),
        }
    }
    flush(&mut buf, &mut toks);
    toks
}

struct Parser {
    toks: Vec<Tok>,
    i: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.i)
    }
    fn eat(&mut self, t: &Tok) -> bool {
        if self.peek() == Some(t) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn or(&mut self) -> Formula {
        let mut v = vec![self.and()];
        while self.eat(&Tok::Or) {
            v.push(self.and());
        }
        if v.len() == 1 {
            v.pop().unwrap()
        } else {
            Formula::Or(v)
        }
    }
    fn and(&mut self) -> Formula {
        let mut v = vec![self.unary()];
        while self.eat(&Tok::And) {
            v.push(self.unary());
        }
        if v.len() == 1 {
            v.pop().unwrap()
        } else {
            Formula::And(v)
        }
    }
    fn unary(&mut self) -> Formula {
        if self.eat(&Tok::Not) {
            Formula::Not(Box::new(self.unary()))
        } else {
            self.primary()
        }
    }
    fn primary(&mut self) -> Formula {
        match self.peek().cloned() {
            Some(Tok::LParen) => {
                self.i += 1;
                let f = self.or();
                self.eat(&Tok::RParen);
                f
            }
            Some(Tok::Atom(a)) => {
                self.i += 1;
                Formula::Atom(a)
            }
            _ => Formula::True,
        }
    }
}

fn parse(clause: &str) -> Formula {
    let toks = tokenize(clause);
    if toks.is_empty() {
        return Formula::True;
    }
    Parser { toks, i: 0 }.or()
}

/// A guard is a list of `when:` clauses, ANDed together.
fn conj(clauses: &[String]) -> Formula {
    Formula::And(clauses.iter().map(|c| parse(c)).collect())
}

fn collect(f: &Formula, out: &mut BTreeSet<String>) {
    match f {
        Formula::Atom(a) => {
            out.insert(a.clone());
        }
        Formula::Not(x) => collect(x, out),
        Formula::And(v) | Formula::Or(v) => v.iter().for_each(|x| collect(x, out)),
        Formula::True => {}
    }
}

fn eval(f: &Formula, val: &impl Fn(&str) -> bool) -> bool {
    match f {
        Formula::True => true,
        Formula::Atom(a) => val(a),
        Formula::Not(x) => !eval(x, val),
        Formula::And(v) => v.iter().all(|x| eval(x, val)),
        Formula::Or(v) => v.iter().any(|x| eval(x, val)),
    }
}

/// If the `use_guard` can hold while none of `def_guards` do, return a description of that
/// case (a coverage gap). `None` if every use-case is covered, or if we can't soundly reason:
///
/// - an *unguarded* use (no `when:`) — nothing to compare against here.
/// - any *unconditional* definition — it covers every case, so no gap.
/// - a definition guarded by an atom the use never mentions — outside the use's vocabulary,
///   so we can't relate them; stay silent rather than risk a false positive.
/// - more atoms than we'll enumerate.
pub fn coverage_gap(use_guard: &[String], def_guards: &[Vec<String>]) -> Option<String> {
    if use_guard.is_empty() || def_guards.is_empty() {
        return None;
    }
    if def_guards.iter().any(|g| g.is_empty()) {
        return None;
    }
    let u = conj(use_guard);
    let d = Formula::Or(def_guards.iter().map(|g| conj(g)).collect());

    let mut u_atoms = BTreeSet::new();
    collect(&u, &mut u_atoms);
    let mut d_atoms = BTreeSet::new();
    collect(&d, &mut d_atoms);
    // Only reason inside the vocabulary the use itself names.
    if !d_atoms.is_subset(&u_atoms) {
        return None;
    }
    let atoms: Vec<String> = u_atoms.into_iter().collect();
    if atoms.is_empty() || atoms.len() > 16 {
        return None;
    }

    for mask in 0u32..(1 << atoms.len()) {
        let val = |a: &str| {
            atoms
                .iter()
                .position(|x| x == a)
                .is_some_and(|i| (mask >> i) & 1 == 1)
        };
        if eval(&u, &val) && !eval(&d, &val) {
            return Some(describe(&atoms, mask));
        }
    }
    None
}

/// Render a counterexample as the condition under which the gap occurs. Prefer the atoms that
/// are *true* (the positive case, e.g. `inventory_hostname == 'web02'`); fall back to the
/// negated form if none are.
fn describe(atoms: &[String], mask: u32) -> String {
    let positives: Vec<String> = atoms
        .iter()
        .enumerate()
        .filter(|(i, _)| (mask >> i) & 1 == 1)
        .map(|(_, a)| a.clone())
        .collect();
    if !positives.is_empty() {
        return positives.join(" and ");
    }
    atoms
        .iter()
        .map(|a| format!("not ({a})"))
        .collect::<Vec<_>>()
        .join(" and ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(s: &str) -> Vec<String> {
        vec![s.to_string()]
    }

    #[test]
    fn use_or_wider_than_single_def_warns() {
        // use: web01 or web02 ; def: web01  -> web02 uncovered
        let gap = coverage_gap(
            &g("inventory_hostname == 'web01' or inventory_hostname == 'web02'"),
            &[g("inventory_hostname == 'web01'")],
        );
        assert!(gap.as_deref().unwrap().contains("web02"));
    }

    #[test]
    fn two_defs_cover_the_or() {
        // use: web01 or web02 ; defs: web01, web02 -> covered
        let gap = coverage_gap(
            &g("inventory_hostname == 'web01' or inventory_hostname == 'web02'"),
            &[
                g("inventory_hostname == 'web01'"),
                g("inventory_hostname == 'web02'"),
            ],
        );
        assert_eq!(gap, None);
    }

    #[test]
    fn same_guard_is_covered() {
        assert_eq!(coverage_gap(&g("enable_x"), &[g("enable_x")]), None);
    }

    #[test]
    fn stronger_use_is_covered() {
        // use: A and B ; def: A -> covered (use implies def)
        assert_eq!(coverage_gap(&g("a and b"), &[g("a")]), None);
    }

    #[test]
    fn stronger_def_leaves_a_gap() {
        // use: A ; def: A and B -> A and not B uncovered. B is in the use vocabulary? No —
        // B isn't in the use, so we stay silent (conservative).
        assert_eq!(coverage_gap(&g("a"), &[g("a and b")]), None);
    }

    #[test]
    fn unconditional_def_covers_everything() {
        assert_eq!(coverage_gap(&g("a"), &[vec![]]), None);
    }

    #[test]
    fn unrelated_vocab_stays_silent() {
        // def guards on something the use never mentions -> can't relate -> silent.
        assert_eq!(coverage_gap(&g("a"), &[g("b")]), None);
    }

    #[test]
    fn unguarded_use_is_not_our_case() {
        assert_eq!(coverage_gap(&[], &[g("a")]), None);
    }
}
