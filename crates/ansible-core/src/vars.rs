//! Where variables are *defined* within a single file — the substrate for variable
//! go-to-definition and the "is this ever set?" checks.
//!
//! This pass covers only the definitions visible in one parsed file: play/block/task
//! `vars:`, `set_fact:`, and `register:`. The cross-file sources — role `defaults/`
//! and `vars/`, `vars_files:`, `include_vars:` — and the opaque ones (inventory,
//! `-e` extra-vars) are separate steps; a name absent here is *not* proof it's undefined.

use crate::ast::{Ast, Block, Play, PlayItem, Stmt, Task};
use crate::condition;
use crate::parse::{Node, Span};

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

/// A place a variable is *used*, with the absolute span of the name.
#[derive(Debug, Clone)]
pub struct VarUse {
    pub name: String,
    pub span: Span,
}

/// Variable uses inside `{{ }}` templates in `text`. `base` is the byte offset of `text`
/// in the document, so the returned spans are absolute. Literal text outside `{{ }}` is
/// not scanned — only a template expression references variables.
pub fn template_uses(text: &str, base: usize, out: &mut Vec<VarUse>) {
    let mut i = 0;
    while let Some(open) = text[i..].find("{{") {
        let expr_start = i + open + 2;
        let Some(close_rel) = text[expr_start..].find("}}") else {
            break;
        };
        let expr = &text[expr_start..expr_start + close_rel];
        for (name, s, e) in condition::variable_uses(expr) {
            out.push(VarUse {
                name,
                span: Span {
                    start: base + expr_start + s,
                    end: base + expr_start + e,
                },
            });
        }
        i = expr_start + close_rel + 2;
    }
}

/// Variable uses in a bare Jinja expression (a `when:` clause), where the whole string is
/// the expression rather than literal text with `{{ }}` islands.
pub fn expression_uses(expr: &str, base: usize, out: &mut Vec<VarUse>) {
    for (name, s, e) in condition::variable_uses(expr) {
        out.push(VarUse {
            name,
            span: Span {
                start: base + s,
                end: base + e,
            },
        });
    }
}

/// Every variable use in a parsed file. A `when:` value is treated as one expression; any
/// other scalar is treated as literal text with `{{ }}` templates. Walks the raw tree so
/// every scalar's span is exact.
pub fn uses(nodes: &[Node]) -> Vec<VarUse> {
    let mut out = Vec::new();
    for n in nodes {
        walk_uses(n, false, &mut out);
    }
    out
}

fn walk_uses(node: &Node, in_when: bool, out: &mut Vec<VarUse>) {
    match node {
        Node::Scalar { value, span } => {
            if in_when {
                expression_uses(value, span.start, out);
            } else {
                template_uses(value, span.start, out);
            }
        }
        Node::Sequence { items, .. } => items.iter().for_each(|i| walk_uses(i, in_when, out)),
        Node::Mapping { entries, .. } => {
            for (k, v) in entries {
                let is_when = k.as_str().map(short_key) == Some("when");
                walk_uses(v, is_when, out);
            }
        }
        Node::Other { .. } => {}
    }
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

    #[test]
    fn template_use_span_is_the_variable_only() {
        let text = "prefix-{{ db_host }}/rest";
        let mut out = Vec::new();
        template_uses(text, 0, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "db_host");
        assert_eq!(out[0].span.slice(text), "db_host");
    }

    #[test]
    fn literal_outside_delimiters_is_not_a_use() {
        let mut out = Vec::new();
        template_uses("just a path.yml", 0, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn uses_treats_when_as_expression_and_values_as_templates() {
        let src = concat!(
            "- hosts: all\n",
            "  tasks:\n",
            "    - debug:\n",
            "        msg: \"{{ greeting }} world\"\n",
            "      when: enabled | default(false)\n",
        );
        let nodes = Document::new(src.to_string()).parse().unwrap();
        let u = uses(&nodes);
        let names: Vec<&str> = u.iter().map(|x| x.name.as_str()).collect();
        assert!(names.contains(&"greeting"), "template var: {names:?}");
        assert!(names.contains(&"enabled"), "when var: {names:?}");
        // `msg` (a key) and `world` (literal) are not variables.
        assert!(!names.contains(&"world"));
        assert!(!names.contains(&"msg"));
        // Spans are exact.
        let g = u.iter().find(|x| x.name == "greeting").unwrap();
        assert_eq!(g.span.slice(src), "greeting");
    }

    #[test]
    fn attribute_and_filter_roots_only() {
        let mut out = Vec::new();
        template_uses("{{ result.stat.exists | default(false) }}", 0, &mut out);
        let names: Vec<&str> = out.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, vec!["result"]);
    }
}
