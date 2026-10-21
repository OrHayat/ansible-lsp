//! Jinja, as Ansible reaches it.
//!
//! ansible-core has two compile paths and `_engine.py:293` picks between them. `when:`,
//! `loop:` and `assert:`'s `that` go to `_compile_expression`, which is
//! `env.compile_expression` — `Parser(state="variable")`, `parse_expression()`, and a check
//! that the stream reached its end. That path cannot contain a statement, so **an expression
//! parser is complete for it**, not a subset of something larger. Everything else — a `.j2`
//! file, and any templated scalar — goes to `_compile_template` and needs the whole document
//! grammar. This module is the first path (T-188); the second is T-040's.
//!
//! [`lexer`] is private on purpose. A token stream is not an answer about Ansible, and the
//! only thing that should be able to ask for one is the parser next to it.

mod ast;
mod highlight;
mod lexer;
mod names;
mod parser;
mod statement;
mod template;

pub use ast::{Args, BinOp, CmpOp, Const, Expr, ExprKind, UnOp};
pub use lexer::{Cause, Error};
pub use names::{expression_lookups, template_lookups, Lookup};
pub use parser::parse;
pub use statement::{references, references_in, will_not_render, will_not_render_in, RefTag, Reference, Stmt};
pub use highlight::{expression_tokens, tokens as semantic_tokens, SemToken, TokenType};
pub use template::{blocks, document, document_in, header, Block, Delimiters, Header, Kind};

/// Whether Ansible treats `s` as a template: `is_possibly_template` (`_jinja_bits.py:973`),
/// any of the three start strings anywhere, or the `#jinja2:` overrides header at the very
/// start. A lightweight check on purpose, as upstream's is — an invalid template is still a
/// template, which fails when rendered rather than being read as plain text, so a parse
/// cannot stand in for this. Playbook values always use the default start strings; only a
/// template file's header can change them.
pub fn is_template(s: &str) -> bool {
    s.starts_with("#jinja2:") || s.contains("{{") || s.contains("{%") || s.contains("{#")
}

#[cfg(test)]
mod tests {
    /// Every row is `is_possibly_template`'s own answer on 2.21.3, run on these strings.
    #[test]
    fn a_template_is_what_ansible_calls_one() {
        let rows = [
            ("plain.yml", false),
            ("{{ x }}.yml", true),
            ("{% if a %}b{% endif %}.yml", true),
            ("a{# c #}.yml", true),
            ("#jinja2: trim_blocks:False\nx", true),
            (" #jinja2:x", false),
            ("x #jinja2:", false),
            ("{ {x}}", false),
            ("{x}", false),
            ("}}", false),
            ("%}", false),
            ("{", false),
            ("a{{", true),
            ("{%", true),
            ("{#", true),
            ("\\{{ x }}", true),
            ("{%- x -%}", true),
        ];
        for (s, want) in rows {
            assert_eq!(super::is_template(s), want, "{s:?}");
        }
    }
}
