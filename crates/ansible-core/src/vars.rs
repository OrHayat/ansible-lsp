//! Where variables are *defined* within a single file — the substrate for variable
//! go-to-definition and the "is this ever set?" checks.
//!
//! This pass covers only the definitions visible in one parsed file: play/block/task
//! `vars:`, `set_fact:`, and `register:`. The cross-file sources — role `defaults/`
//! and `vars/`, `vars_files:`, `include_vars:` — and the opaque ones (inventory,
//! `-e` extra-vars) are separate steps; a name absent here is *not* proof it's undefined.

use crate::ast::{Ast, Block, Play, PlayItem, Stmt, Task};
use crate::parse::Span;

/// How a variable came to be defined. Ordered loosely by Ansible's precedence, low to
/// high, though this pass doesn't yet rank across files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarSource {
    /// A play `vars:` entry.
    PlayVars,
    /// A block `vars:` entry.
    BlockVars,
    /// A task `vars:` entry.
    TaskVars,
    /// A `set_fact:` key.
    SetFact,
    /// A `register:` name.
    Register,
}

#[derive(Debug, Clone)]
pub struct VarDef {
    pub name: String,
    pub source: VarSource,
    /// Span to jump to — the value of the binding, the fact key, or the register name.
    pub span: Span,
}

/// Every in-file variable definition, in document order.
#[derive(Debug, Default)]
pub struct VarIndex {
    defs: Vec<VarDef>,
}

impl VarIndex {
    pub fn defs(&self) -> &[VarDef] {
        &self.defs
    }

    /// Every definition of `name` (a variable can be set more than once).
    pub fn get(&self, name: &str) -> Vec<&VarDef> {
        self.defs.iter().filter(|d| d.name == name).collect()
    }

    fn push(&mut self, name: impl Into<String>, source: VarSource, span: Span) {
        self.defs.push(VarDef {
            name: name.into(),
            source,
            span,
        });
    }
}

fn short_key(key: &str) -> &str {
    key.rsplit('.').next().unwrap_or(key)
}

/// Build the variable-definition index for one parsed file.
pub fn index(tree: &Ast) -> VarIndex {
    let mut idx = VarIndex::default();
    match tree {
        Ast::Playbook(items) => {
            for it in items {
                if let PlayItem::Play(p) = it {
                    play(p, &mut idx);
                }
            }
        }
        Ast::Tasks(stmts) => stmts.iter().for_each(|s| stmt(s, &mut idx)),
        Ast::Other => {}
    }
    idx
}

fn play(p: &Play, idx: &mut VarIndex) {
    for v in &p.vars {
        idx.push(v.name.clone(), VarSource::PlayVars, v.span);
    }
    for s in p
        .pre_tasks
        .iter()
        .chain(&p.tasks)
        .chain(&p.post_tasks)
        .chain(&p.handlers)
    {
        stmt(s, idx);
    }
}

fn stmt(s: &Stmt, idx: &mut VarIndex) {
    match s {
        Stmt::Task(t) => task(t, idx),
        Stmt::Block(b) => block(b, idx),
    }
}

fn block(b: &Block, idx: &mut VarIndex) {
    for v in &b.vars {
        idx.push(v.name.clone(), VarSource::BlockVars, v.span);
    }
    for s in b.block.iter().chain(&b.rescue).chain(&b.always) {
        stmt(s, idx);
    }
}

fn task(t: &Task, idx: &mut VarIndex) {
    for v in &t.vars {
        idx.push(v.name.clone(), VarSource::TaskVars, v.span);
    }
    if let Some(a) = &t.action {
        if short_key(&a.name) == "set_fact" {
            for (fact, _) in a.args.entries() {
                if let Some(name) = fact.as_str() {
                    // `cacheable` is a set_fact option, not a fact.
                    if name != "cacheable" {
                        idx.push(name, VarSource::SetFact, fact.span());
                    }
                }
            }
        }
    }
    if let (Some(name), Some(span)) = (&t.register, t.register_span) {
        idx.push(name.clone(), VarSource::Register, span);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast;
    use crate::parse::Document;

    fn idx(src: &str) -> VarIndex {
        index(&ast::build(&Document::new(src.to_string()).parse().expect("valid yaml")))
    }

    #[test]
    fn collects_play_and_task_and_set_fact_and_register() {
        let i = idx(concat!(
            "- hosts: all\n",
            "  vars:\n",
            "    play_var: 1\n",
            "  tasks:\n",
            "    - name: t\n",
            "      command: echo\n",
            "      vars:\n",
            "        task_var: 2\n",
            "      register: out\n",
            "    - set_fact:\n",
            "        made: true\n",
            "        cacheable: yes\n",
        ));
        let src = |n: &str| i.get(n).first().map(|d| d.source);
        assert_eq!(src("play_var"), Some(VarSource::PlayVars));
        assert_eq!(src("task_var"), Some(VarSource::TaskVars));
        assert_eq!(src("out"), Some(VarSource::Register));
        assert_eq!(src("made"), Some(VarSource::SetFact));
        // `cacheable` is an option of set_fact, not a fact it defines.
        assert!(i.get("cacheable").is_empty());
    }

    #[test]
    fn block_vars_and_nested_tasks() {
        let i = idx(concat!(
            "- hosts: all\n",
            "  tasks:\n",
            "    - block:\n",
            "        - set_fact:\n",
            "            inner: 1\n",
            "      vars:\n",
            "        block_var: 2\n",
        ));
        assert_eq!(i.get("block_var").first().map(|d| d.source), Some(VarSource::BlockVars));
        assert_eq!(i.get("inner").first().map(|d| d.source), Some(VarSource::SetFact));
    }

    #[test]
    fn span_points_at_the_definition() {
        let src = "- hosts: all\n  vars:\n    db_host: db01.internal\n";
        let i = index(&ast::build(&Document::new(src.to_string()).parse().unwrap()));
        assert_eq!(i.get("db_host")[0].span.slice(src), "db01.internal");
    }

    #[test]
    fn task_file_register_is_indexed() {
        let i = idx("- command: echo hi\n  register: result\n");
        assert_eq!(i.get("result").first().map(|d| d.source), Some(VarSource::Register));
    }

    #[test]
    fn same_name_set_twice_yields_two_defs() {
        let i = idx(
            "- hosts: all\n  tasks:\n    - set_fact: { x: 1 }\n    - set_fact: { x: 2 }\n",
        );
        assert_eq!(i.get("x").len(), 2);
    }
}
