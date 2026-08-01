//! Where variables are *defined* — the substrate for variable go-to-definition and the
//! "is this ever set?" checks.
//!
//! [`index`] covers one parsed file: play/block/task `vars:`, `set_fact:`, `register:`.
//! [`definitions`] extends that across files by deterministic paths only — role
//! `defaults/`/`vars/`, `vars_files:`, playbook-adjacent `group_vars/`/`host_vars/`, and the
//! `set_fact`/`register` in included task files and roles. Still not covered: `include_vars:`,
//! group_vars/host_vars kept beside a *separate inventory file* (needs the inventory's path,
//! not guessed), variables injected by a caller, and the opaque runtime sources (inventory
//! host-matching, `-e`). So a name absent here is *not* proof it's undefined.

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    /// A key in a playbook-adjacent `group_vars/all` file — applies to every host.
    GroupVarsAll,
    /// A key in a playbook-adjacent `group_vars/<group>` file — applies only to hosts in
    /// that group, which is host-dependent.
    GroupVars,
    /// A key in a playbook-adjacent `host_vars/<host>` file — applies only to that host.
    HostVars,
    /// A key loaded by an `include_vars:` task (file or dir form).
    IncludeVars,
}

impl VarSource {
    /// Ansible's variable-precedence level (higher wins). Only the sources we index; the
    /// inventory-adjacent copies (next to a separate inventory file) and extra-vars (22)
    /// aren't here.
    pub fn precedence(self) -> u8 {
        match self {
            VarSource::RoleDefaults => 2,
            VarSource::GroupVarsAll => 5,
            VarSource::GroupVars => 7,
            VarSource::HostVars => 10,
            VarSource::PlayVars => 12,
            VarSource::VarsFiles => 14,
            VarSource::RoleVars => 15,
            VarSource::BlockVars => 16,
            VarSource::TaskVars => 17,
            VarSource::IncludeVars => 18,
            VarSource::SetFact | VarSource::Register => 19,
        }
    }

    /// True for sources whose applicability depends on the target host — a named `group_vars`
    /// or `host_vars` file. We can point at the definition, but not assert it's in effect for
    /// a given host without parsing inventory.
    pub fn host_scoped(self) -> bool {
        matches!(self, VarSource::GroupVars | VarSource::HostVars)
    }
}

#[derive(Debug, Clone)]
pub struct VarDef {
    pub name: String,
    pub source: VarSource,
    /// Span to jump to — the value of the binding, the fact key, or the register name.
    pub span: Span,
    /// The `when:` guarding the defining task, if any — so a conditionally set/registered
    /// variable reads as "defined only when …" (e.g. per-host via `inventory_hostname`),
    /// straight from the playbook, no inventory needed.
    pub condition: Option<String>,
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
        self.push_cond(name, source, span, None);
    }

    fn push_cond(
        &mut self,
        name: impl Into<String>,
        source: VarSource,
        span: Span,
        condition: Option<String>,
    ) {
        self.defs.push(VarDef {
            name: name.into(),
            source,
            span,
            condition,
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
    /// The `when:` clauses guarding the task this use sits in (accumulated through nesting),
    /// so a conditional use can be checked against its definitions' conditions.
    pub guard: Vec<String>,
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
                guard: Vec::new(),
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
            guard: Vec::new(),
        });
    }
}

/// Every variable use in a parsed file. A `when:` value is treated as one expression; any
/// other scalar is treated as literal text with `{{ }}` templates. Walks the raw tree so
/// every scalar's span is exact, and accumulates the enclosing `when:` onto each use.
pub fn uses(nodes: &[Node]) -> Vec<VarUse> {
    let mut out = Vec::new();
    for n in nodes {
        walk_uses(n, false, &[], &mut out);
    }
    out
}

/// The `when:` clauses on a mapping (a task/block), if any.
fn when_of(node: &Node) -> Vec<String> {
    match node.get("when") {
        Some(Node::Sequence { items, .. }) => items
            .iter()
            .filter_map(|i| i.as_str().map(str::to_owned))
            .collect(),
        Some(other) => other.as_str().map(str::to_owned).into_iter().collect(),
        None => Vec::new(),
    }
}

fn walk_uses(node: &Node, in_when: bool, guard: &[String], out: &mut Vec<VarUse>) {
    match node {
        Node::Scalar { value, span } => {
            let before = out.len();
            if in_when {
                expression_uses(value, span.start, out);
            } else {
                template_uses(value, span.start, out);
            }
            for u in &mut out[before..] {
                u.guard = guard.to_vec();
            }
        }
        Node::Sequence { items, .. } => {
            items.iter().for_each(|i| walk_uses(i, in_when, guard, out))
        }
        Node::Mapping { entries, .. } => {
            // This task/block's own `when:` guards the values inside it (its module args),
            // accumulated onto whatever guard we inherited.
            let mut inner = guard.to_vec();
            inner.extend(when_of(node));
            for (k, v) in entries {
                let is_when = k.as_str().map(short_key) == Some("when");
                // The `when:` expression itself isn't guarded by itself — use the outer guard.
                let g: &[String] = if is_when { guard } else { &inner };
                walk_uses(v, is_when, g, out);
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
    // A task's `when:` guards everything it defines — so the variable is only set on the
    // hosts/runs where the condition holds.
    let cond = (!t.when.is_empty()).then(|| t.when.join(" and "));
    for v in &t.vars {
        idx.push_cond(v.name.clone(), VarSource::TaskVars, v.span, cond.clone());
    }
    if let Some(a) = &t.action {
        if short_key(&a.name) == "set_fact" {
            for (fact, _) in a.args.entries() {
                if let Some(name) = fact.as_str() {
                    // `cacheable` is a set_fact option, not a fact.
                    if name != "cacheable" {
                        idx.push_cond(name, VarSource::SetFact, fact.span(), cond.clone());
                    }
                }
            }
        }
    }
    if let (Some(name), Some(span)) = (&t.register, t.register_span) {
        idx.push_cond(name.clone(), VarSource::Register, span, cond.clone());
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
    /// The `when:` guarding the defining task, if any (see [`VarDef::condition`]).
    pub condition: Option<String>,
}

impl Located {
    /// Whether this definition can be in effect at a use at byte `use_pos` in `use_file`.
    ///
    /// Play/block/task `vars:`, `vars_files:` and role defaults/vars bind before the tasks
    /// run, so they always apply. `set_fact`/`register` happen at a point in the run: in the
    /// *same* file, one after the use hasn't executed yet, so it can't define that use. Across
    /// files we can't order it against the use, so we keep it rather than guess.
    pub fn in_effect_at(&self, use_file: &Path, use_pos: usize) -> bool {
        match self.source {
            VarSource::SetFact | VarSource::Register => {
                self.file != use_file || self.span.start < use_pos
            }
            _ => true,
        }
    }
}

/// The definition that wins among `defs` — which must already be filtered to one name and
/// to those in effect at the use. Highest precedence; ties broken by latest position (the
/// last assignment wins). `None` if empty. Caveat: `-e` (and, only when the winner is a role
/// default, inventory) can still override at runtime — those aren't indexed.
pub fn effective(defs: &[Located]) -> Option<&Located> {
    defs.iter().max_by(|a, b| {
        a.source
            .precedence()
            .cmp(&b.source.precedence())
            .then(a.span.start.cmp(&b.span.start))
    })
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
            condition: d.condition.clone(),
        });
    }

    // The enclosing role's defaults/ and vars/ — fixed locations, no search.
    if let Some(role) = &ctx.role_dir {
        read_var_file(&role.join("defaults").join("main.yml"), VarSource::RoleDefaults, None, out);
        read_var_file(&role.join("vars").join("main.yml"), VarSource::RoleVars, None, out);
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
                        read_var_file(&f, VarSource::VarsFiles, None, out);
                    }
                }
            }
        }
    }

    // include_vars tasks — load a file or a directory at a point in the play. The task's
    // when: guards the load, so it carries a condition. Templated targets are skipped.
    each_task(&tree, &mut |t| {
        let Some(a) = &t.action else { return };
        if short_key(&a.name) != "include_vars" {
            return;
        }
        let cond = (!t.when.is_empty()).then(|| t.when.join(" and "));
        if let Some(dir) = a.args.get("dir").and_then(|n| n.as_str()) {
            if !dir.contains("{{") {
                if let Some(d) = resolve_dir_path(dir, &ctx) {
                    read_dir_files(&d, VarSource::IncludeVars, cond, out);
                }
            }
            return;
        }
        let file = match &a.args {
            Node::Scalar { value, .. } => Some(value.as_str()),
            Node::Mapping { .. } => a.args.get("file").and_then(|n| n.as_str()),
            _ => None,
        };
        if let Some(f) = file {
            if !f.contains("{{") {
                if let Some(path) = resolve_var_path(f, &ctx) {
                    read_var_file(&path, VarSource::IncludeVars, cond, out);
                }
            }
        }
    });

    // Playbook-adjacent group_vars/ and host_vars/ — a fixed location next to this file, not
    // a workspace scan. The inventory-adjacent copies (next to a separate inventory file)
    // need the inventory's location, which we don't guess, so those stay unindexed.
    read_var_dir(&ctx.file_dir.join("group_vars"), true, out);
    read_var_dir(&ctx.file_dir.join("host_vars"), false, out);

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

/// Read every `*.yml` in a playbook-adjacent `group_vars/` or `host_vars/` directory. The
/// source is derived from the file name: `group_vars/all` applies to all hosts, any other
/// name is group- or host-scoped.
fn read_var_dir(dir: &Path, group: bool, out: &mut Vec<Located>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if !matches!(
            p.extension().and_then(|s| s.to_str()),
            Some("yml") | Some("yaml")
        ) {
            continue;
        }
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let source = if !group {
            VarSource::HostVars
        } else if stem == "all" {
            VarSource::GroupVarsAll
        } else {
            VarSource::GroupVars
        };
        read_var_file(&p, source, None, out);
    }
}

/// Read every `*.yml` in a directory as one source — the `include_vars: { dir: … }` form.
fn read_dir_files(dir: &Path, source: VarSource, condition: Option<String>, out: &mut Vec<Located>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if matches!(
            p.extension().and_then(|s| s.to_str()),
            Some("yml") | Some("yaml")
        ) {
            read_var_file(&p, source, condition.clone(), out);
        }
    }
}

/// Read a flat `name: value` vars file and index every top-level key. `condition` is the
/// `when:` guarding the load, if any (only `include_vars`, a task, can carry one).
fn read_var_file(file: &Path, source: VarSource, condition: Option<String>, out: &mut Vec<Located>) {
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
                        condition: condition.clone(),
                    });
                }
            }
        }
    }
}

/// Visit every task in a parsed file, descending into blocks and plays.
fn each_task(tree: &Ast, f: &mut impl FnMut(&Task)) {
    fn stmt(s: &Stmt, f: &mut impl FnMut(&Task)) {
        match s {
            Stmt::Task(t) => f(t),
            Stmt::Block(b) => b
                .block
                .iter()
                .chain(&b.rescue)
                .chain(&b.always)
                .for_each(|s| stmt(s, f)),
        }
    }
    match tree {
        Ast::Playbook(items) => {
            for it in items {
                if let PlayItem::Play(p) = it {
                    p.pre_tasks
                        .iter()
                        .chain(&p.tasks)
                        .chain(&p.post_tasks)
                        .chain(&p.handlers)
                        .for_each(|s| stmt(s, f));
                }
            }
        }
        Ast::Tasks(stmts) => stmts.iter().for_each(|s| stmt(s, f)),
        Ast::Other => {}
    }
}

/// Resolve a directory reference for `include_vars: { dir: … }`, first that exists.
fn resolve_dir_path(entry: &str, ctx: &FileContext) -> Option<PathBuf> {
    let mut cands = vec![ctx.file_dir.join(entry)];
    if let Some(role) = &ctx.role_dir {
        cands.push(role.join(entry));
        cands.push(role.join("vars").join(entry));
    }
    if let Some(root) = &ctx.project_root {
        cands.push(root.join(entry));
    }
    cands.into_iter().find(|p| p.is_dir())
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

    #[test]
    fn a_use_carries_its_task_when_as_its_guard() {
        let src = concat!(
            "- hosts: all\n",
            "  tasks:\n",
            "    - debug: { msg: \"{{ x }}\" }\n",
            "      when: inventory_hostname == 'web01'\n",
            "    - debug: { msg: \"{{ y }}\" }\n",
        );
        let nodes = Document::new(src.to_string()).parse().unwrap();
        let u = uses(&nodes);
        let x = u.iter().find(|u| u.name == "x").unwrap();
        assert_eq!(x.guard, vec!["inventory_hostname == 'web01'".to_string()]);
        // y's task has no when:, so no guard.
        let y = u.iter().find(|u| u.name == "y").unwrap();
        assert!(y.guard.is_empty());
    }

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
    }

    #[test]
    fn effective_picks_highest_precedence_then_latest() {
        let mk = |src, start| Located {
            name: "x".into(),
            source: src,
            span: Span { start, end: start + 1 },
            file: PathBuf::from("f.yml"),
            condition: None,
        };
        // set_fact (19) beats a play var (12) regardless of position.
        let defs = vec![mk(VarSource::PlayVars, 10), mk(VarSource::SetFact, 5)];
        assert_eq!(effective(&defs).unwrap().source, VarSource::SetFact);
        // Two set_facts: the later one wins.
        let defs = vec![mk(VarSource::SetFact, 5), mk(VarSource::SetFact, 90)];
        assert_eq!(effective(&defs).unwrap().span.start, 90);
        assert!(effective(&[]).is_none());
    }

    #[test]
    fn set_fact_after_a_use_is_not_in_effect() {
        let file = Path::new("play.yml");
        let sf = Located {
            name: "x".into(),
            source: VarSource::SetFact,
            span: Span { start: 100, end: 110 },
            file: file.to_path_buf(),
            condition: None,
        };
        // A use before the set_fact: not yet defined by it.
        assert!(!sf.in_effect_at(file, 50));
        // A use after it: in effect.
        assert!(sf.in_effect_at(file, 150));
        // A set_fact in another file can't be ordered against this use — kept.
        assert!(sf.in_effect_at(Path::new("other.yml"), 50));
        // Play vars bind before tasks, so position doesn't matter.
        let pv = Located { source: VarSource::PlayVars, ..sf.clone() };
        assert!(pv.in_effect_at(file, 50));
    }

    #[test]
    fn include_vars_file_and_dir_forms_are_indexed() {
        let d = std::env::temp_dir().join("ansible-lsp-incvars");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write(&d, "vars/db.yml", "db_pool: 20\n");
        write(&d, "conf/a.yml", "conf_a: 1\n");
        write(&d, "conf/b.yml", "conf_b: 2\n");
        let play = d.join("play.yml");
        std::fs::write(
            &play,
            concat!(
                "- hosts: all\n",
                "  tasks:\n",
                "    - include_vars: vars/db.yml\n",
                "    - include_vars: { dir: conf }\n",
                "    - include_vars: \"{{ x }}.yml\"\n",
            ),
        )
        .unwrap();
        let nodes = Document::new(std::fs::read_to_string(&play).unwrap())
            .parse()
            .unwrap();
        let defs = definitions(&play, &nodes);
        let src = |n: &str| defs.iter().find(|x| x.name == n).map(|x| x.source);
        assert_eq!(src("db_pool"), Some(VarSource::IncludeVars)); // file form
        assert_eq!(src("conf_a"), Some(VarSource::IncludeVars)); // dir form
        assert_eq!(src("conf_b"), Some(VarSource::IncludeVars));
        // Templated target is skipped, not guessed.
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

        // prov_user is now layered: role default (2) < group_vars/all (5) < host_vars (10),
        // both playbook-adjacent.
        write(&d, "group_vars/all.yml", "prov_user: from_group\n");
        write(&d, "host_vars/web01.yml", "prov_user: from_host\n");

        let nodes = Document::new(std::fs::read_to_string(&play).unwrap())
            .parse()
            .unwrap();
        let defs = definitions(&play, &nodes);
        let src = |n: &str| defs.iter().find(|x| x.name == n).map(|x| x.source);
        assert_eq!(src("shared_endpoint"), Some(VarSource::VarsFiles));
        assert_eq!(src("prov_ready"), Some(VarSource::SetFact));
        // prov_user has all three layers, and host_vars wins by precedence.
        let user_defs: Vec<_> = defs.iter().filter(|d| d.name == "prov_user").cloned().collect();
        let sources: HashSet<VarSource> = user_defs.iter().map(|d| d.source).collect();
        assert!(sources.contains(&VarSource::RoleDefaults));
        assert!(sources.contains(&VarSource::GroupVarsAll));
        assert!(sources.contains(&VarSource::HostVars));
        assert_eq!(effective(&user_defs).unwrap().source, VarSource::HostVars);
        // Each points at the file it actually lives in.
        assert!(defs
            .iter()
            .find(|x| x.name == "shared_endpoint")
            .unwrap()
            .file
            .ends_with("vars/shared.yml"));
    }
}
