//! Where variables are *defined* — the substrate for variable go-to-definition and the
//! "is this ever set?" checks.
//!
//! [`index`] covers one parsed file: play/block/task `vars:`, `set_fact:`, `register:`.
//! [`definitions`] extends that across files by deterministic paths only — role
//! `defaults/`/`vars/`, `vars_files:`, and the `set_fact`/`register` in included task files
//! and roles. Still not covered: `include_vars:`, group_vars/host_vars (ambiguous folder,
//! deliberately not scanned), variables injected by a caller, and the opaque runtime
//! sources (inventory, `-e`). So a name absent here is *not* proof it's undefined.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::ast::{self, Ast, Block, Play, PlayItem, Stmt, Task};
use crate::condition;
use crate::parse::{Document, Node, Span};
use crate::references::{self, ReferenceKind};
use crate::resolve;
use crate::workspace::{yaml_files, FileContext};

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
    /// A key in a `vars_files:` target.
    VarsFiles,
    /// A key in the enclosing role's `defaults/main.yml`.
    RoleDefaults,
    /// A key in the enclosing role's `vars/main.yml`.
    RoleVars,
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

// ---------------------------------------------------------------- cross-file

/// A variable definition together with the file it lives in — for cross-file
/// go-to-definition. The byte span is within `file`.
#[derive(Debug, Clone)]
pub struct Located {
    pub name: String,
    pub source: VarSource,
    pub span: Span,
    pub file: PathBuf,
}

/// How far to follow includes/roles when gathering set_fact/register. Matches
/// [`crate::mutation`]'s cap; the definitions that matter are one or two hops away.
const MAX_DEPTH: usize = 4;

/// Every variable definition discoverable *from* `path`: its own in-file definitions, the
/// role `defaults/`+`vars/` and `vars_files:` it pulls in, and the `set_fact`/`register`/
/// `vars:` in the task files and roles it includes. Deterministic paths only — no folder
/// scanning and no inventory. `nodes` is the (possibly unsaved) parse of `path`; deeper
/// files are read from disk.
///
/// Not exhaustive: variables injected by a *caller* (a playbook that includes this file and
/// passes vars), plus inventory and `-e`, aren't visible here — so absence is not proof a
/// variable is undefined.
pub fn definitions(path: &Path, nodes: &[Node]) -> Vec<Located> {
    let mut out = Vec::new();
    let mut visited = HashSet::new();
    if let Ok(c) = path.canonicalize() {
        visited.insert(c);
    }
    collect(path, nodes, 0, &mut out, &mut visited);
    // Same var reached by two paths (e.g. a vars file two plays share) collapses.
    let mut seen = HashSet::new();
    out.retain(|d| seen.insert((d.name.clone(), d.file.clone(), d.span.start)));
    out
}

fn collect(
    path: &Path,
    nodes: &[Node],
    depth: usize,
    out: &mut Vec<Located>,
    visited: &mut HashSet<PathBuf>,
) {
    let ctx = FileContext::discover(path);
    let tree = ast::build(nodes);

    // In-file definitions (play/block/task vars, set_fact, register).
    for d in index(&tree).defs() {
        out.push(Located {
            name: d.name.clone(),
            source: d.source,
            span: d.span,
            file: path.to_path_buf(),
        });
    }

    // The enclosing role's defaults/ and vars/ — fixed locations, no search.
    if let Some(role) = &ctx.role_dir {
        read_var_file(&role.join("defaults").join("main.yml"), VarSource::RoleDefaults, out);
        read_var_file(&role.join("vars").join("main.yml"), VarSource::RoleVars, out);
    }

    // Play-level vars_files — explicit paths written in the play.
    if let Ast::Playbook(items) = &tree {
        for it in items {
            if let PlayItem::Play(p) = it {
                for (entry, _) in &p.vars_files {
                    if entry.contains("{{") {
                        continue;
                    }
                    if let Some(f) = resolve_var_path(entry, &ctx) {
                        read_var_file(&f, VarSource::VarsFiles, out);
                    }
                }
            }
        }
    }

    // Follow includes and roles so set_fact/register/vars in those files count too. The
    // enclosing-role rule above then also picks up each reached role's defaults/vars.
    if depth < MAX_DEPTH {
        for r in references::extract(nodes) {
            if !matches!(
                r.kind,
                ReferenceKind::Role
                    | ReferenceKind::IncludeTasks
                    | ReferenceKind::ImportTasks
                    | ReferenceKind::TasksFrom
                    | ReferenceKind::ImportPlaybook
            ) {
                continue;
            }
            for target in resolve::resolve(&r, &ctx).targets {
                if r.kind == ReferenceKind::Role {
                    for f in role_task_files(&target) {
                        collect_disk(&f, depth + 1, out, visited);
                    }
                } else {
                    collect_disk(&target, depth + 1, out, visited);
                }
            }
        }
    }
}

fn collect_disk(path: &Path, depth: usize, out: &mut Vec<Located>, visited: &mut HashSet<PathBuf>) {
    let Ok(canon) = path.canonicalize() else { return };
    if !visited.insert(canon) {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let Some(nodes) = Document::new(text).parse() else {
        return;
    };
    collect(path, &nodes, depth, out, visited);
}

/// A role contributes every task file it has (`tasks_from` reaches beyond `main.yml`).
fn role_task_files(role_main: &Path) -> Vec<PathBuf> {
    match role_main.parent() {
        Some(tasks_dir) => yaml_files(tasks_dir),
        None => vec![role_main.to_path_buf()],
    }
}

/// Read a flat `name: value` vars file and index every top-level key.
fn read_var_file(file: &Path, source: VarSource, out: &mut Vec<Located>) {
    let Ok(text) = std::fs::read_to_string(file) else {
        return;
    };
    let Some(nodes) = Document::new(text).parse() else {
        return;
    };
    for n in &nodes {
        if let Node::Mapping { entries, .. } = n {
            for (k, v) in entries {
                if let Some(name) = k.as_str() {
                    out.push(Located {
                        name: name.to_string(),
                        source,
                        span: v.span(),
                        file: file.to_path_buf(),
                    });
                }
            }
        }
    }
}

/// Resolve a `vars_files:` entry against the file dir, its `vars/`, the role `vars/` and the
/// project root — where Ansible looks — returning the first path that exists.
fn resolve_var_path(entry: &str, ctx: &FileContext) -> Option<PathBuf> {
    let mut cands = vec![ctx.file_dir.join(entry), ctx.file_dir.join("vars").join(entry)];
    if let Some(role) = &ctx.role_dir {
        cands.push(role.join("vars").join(entry));
    }
    if let Some(root) = &ctx.project_root {
        cands.push(root.join(entry));
    }
    cands.into_iter().find(|p| p.is_file())
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

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
    }

    #[test]
    fn definitions_span_vars_files_role_defaults_and_role_set_fact() {
        let d = std::env::temp_dir().join("ansible-lsp-xfile");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write(&d, "vars/shared.yml", "shared_endpoint: https://x\n");
        write(&d, "roles/prov/defaults/main.yml", "prov_user: deploy\n");
        write(
            &d,
            "roles/prov/tasks/main.yml",
            "- set_fact:\n    prov_ready: true\n",
        );
        let play = d.join("play.yml");
        std::fs::write(
            &play,
            concat!(
                "- hosts: all\n",
                "  vars_files:\n",
                "    - vars/shared.yml\n",
                "  roles:\n",
                "    - prov\n",
                "  tasks:\n",
                "    - debug: { msg: hi }\n",
            ),
        )
        .unwrap();

        let nodes = Document::new(std::fs::read_to_string(&play).unwrap())
            .parse()
            .unwrap();
        let defs = definitions(&play, &nodes);
        let src = |n: &str| defs.iter().find(|x| x.name == n).map(|x| x.source);
        assert_eq!(src("shared_endpoint"), Some(VarSource::VarsFiles));
        assert_eq!(src("prov_user"), Some(VarSource::RoleDefaults));
        assert_eq!(src("prov_ready"), Some(VarSource::SetFact));
        // Each points at the file it actually lives in.
        let f = |n: &str| defs.iter().find(|x| x.name == n).map(|x| x.file.clone());
        assert!(f("shared_endpoint").unwrap().ends_with("vars/shared.yml"));
        assert!(f("prov_user").unwrap().ends_with("roles/prov/defaults/main.yml"));
    }
}
