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
mod lexer;
mod parser;
mod statement;
mod template;

pub use ast::{Args, BinOp, CmpOp, Const, Expr, ExprKind, UnOp};
pub use lexer::{Cause, Error};
pub use parser::parse;
pub use statement::{references, references_in, will_not_render, will_not_render_in, RefTag, Reference, Stmt};
pub use template::{blocks, document, document_in, header, Block, Delimiters, Header, Kind};
