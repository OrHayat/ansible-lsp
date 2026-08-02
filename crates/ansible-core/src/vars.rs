//! Where variables are *defined* — the substrate for variable go-to-definition and the
//! "is this ever set?" checks.
//!
//! [`index`] covers one parsed file: play/block/task `vars:`, `set_fact:`, `register:`.
//! [`definitions`] extends that across files by deterministic paths only — role
//! `defaults/`/`vars/`, `vars_files:`, `include_vars:` (file and dir forms, via the ported
//! module semantics in [`crate::include_vars`]), playbook-adjacent
//! `group_vars/`/`host_vars/`, and the `set_fact`/`register` in included task files and
//! roles. Still not covered: group_vars/host_vars kept beside a *separate inventory file*
//! (needs the inventory's path, not guessed), variables injected by a caller, and the
//! opaque runtime sources (inventory host-matching, `-e`). So a name absent here is *not*
//! proof it's undefined.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::ast::{self, Ast, Block, Play, PlayItem, Stmt, Task};
use crate::condition;
use crate::include_vars;
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

/// Sources whose span points at the variable's *value* (not its name), and whose value isn't
/// host-dependent — so their literal can be read for path substitution (T-056).
fn value_span_source(s: VarSource) -> bool {
    matches!(
        s,
        VarSource::PlayVars
            | VarSource::BlockVars
            | VarSource::TaskVars
            | VarSource::VarsFiles
            | VarSource::RoleDefaults
            | VarSource::RoleVars
            | VarSource::GroupVarsAll
            | VarSource::IncludeVars
    )
}

/// Statically-knowable literal values per variable name, for expanding `{{ var }}` in paths
/// (T-056). Only host-independent, value-span sources with a plain (non-templated) literal —
/// never `set_fact`/`register` (name span), `host_vars`/named `group_vars` (host-dependent),
/// or a value that is itself templated. `text` is the in-memory source of `path`, so
/// same-file spans slice correctly; other files are read from disk.
pub fn known_literals(
    defs: &[Located],
    path: &Path,
    text: &str,
) -> std::collections::HashMap<String, Vec<String>> {
    use std::collections::HashMap;
    let mut disk: HashMap<PathBuf, String> = HashMap::new();
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for d in defs {
        if !value_span_source(d.source) {
            continue;
        }
        let value = {
            let src: &str = if d.file == path {
                text
            } else {
                disk.entry(d.file.clone())
                    .or_insert_with(|| std::fs::read_to_string(&d.file).unwrap_or_default())
            };
            d.span.slice(src).trim().to_string()
        };
        if value.is_empty() || value.contains("{{") {
            continue;
        }
        let vals = out.entry(d.name.clone()).or_default();
        if !vals.contains(&value) {
            vals.push(value);
        }
    }
    out
}

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
    definitions_with_deps(path, nodes).0
}

/// T-051's base case: variable uses with **no reachable definition at all**. The most
/// dangerous diagnostic in the tool — a false "undefined" trains people to ignore the
/// squiggle — so it is conservative to the point of near-silence:
///
/// - playbooks only: a tasks/role file can receive vars from any caller
/// - a definition anywhere reachable exempts, even one that runs later (ordering is the
///   uncovered-`when` check's business, not this one's)
/// - magic vars, `ansible_*` facts, `loop_var`/`vars_prompt`/`{% set %}` declarations
///   are never flagged
/// - a use whose own expression or guard handles undefinedness (`default(…)`,
///   `is defined`) is the author saying "I know" — silent
///
/// The caller owns the message; it must concede inventory, facts and `-e`, which are
/// invisible here.
pub fn undefined_uses(path: &Path, nodes: &[Node], text: &str) -> Vec<VarUse> {
    let tree = ast::build(nodes);
    if !matches!(tree, Ast::Playbook(_)) {
        return Vec::new();
    }
    let defined: HashSet<String> = definitions(path, nodes).into_iter().map(|d| d.name).collect();
    let declared = declared_names(nodes, text);
    uses(nodes)
        .into_iter()
        .filter(|u| {
            !defined.contains(&u.name)
                && !condition::is_magic(&u.name)
                && !u.name.starts_with("ansible_")
                && !declared.contains(&u.name)
                && !u.guard.iter().any(|g| g.contains(&u.name) && g.contains("defined"))
                && !softened(text, u.span.start)
        })
        .collect()
}

/// Names declared by constructs the definition index doesn't model: `loop_control:
/// loop_var`, `vars_prompt:`, and `{% set %}`. Collected file-wide — broader than their
/// real scope, which errs toward silence.
fn declared_names(nodes: &[Node], text: &str) -> HashSet<String> {
    fn walk(n: &Node, out: &mut HashSet<String>) {
        match n {
            Node::Mapping { entries, .. } => {
                for (k, v) in entries {
                    match k.as_str() {
                        Some("loop_control") => {
                            if let Some(lv) = v.get("loop_var").and_then(|x| x.as_str()) {
                                out.insert(lv.to_string());
                            }
                        }
                        Some("vars_prompt") => {
                            for item in v.items() {
                                if let Some(nm) = item.get("name").and_then(|x| x.as_str()) {
                                    out.insert(nm.to_string());
                                }
                            }
                        }
                        _ => {}
                    }
                    walk(v, out);
                }
            }
            Node::Sequence { .. } => n.items().iter().for_each(|i| walk(i, out)),
            _ => {}
        }
    }
    let mut out = HashSet::new();
    nodes.iter().for_each(|n| walk(n, &mut out));
    let mut rest = text;
    while let Some(i) = rest.find("{%") {
        rest = &rest[i + 2..];
        if let Some(after) = rest.trim_start().strip_prefix("set ") {
            let name: String = after
                .trim_start()
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                out.insert(name);
            }
        }
    }
    out
}

/// Does the expression around byte `at` handle undefinedness itself? Looks at the
/// enclosing `{{ … }}`, or the enclosing line for a bare `when:` expression, for
/// `default(` or an `is (not) defined` test. Text-level and deliberately loose — a false
/// "handled" is silence, the safe direction.
fn softened(text: &str, at: usize) -> bool {
    // Use spans are value-relative offsets rebased onto the source; block and escaped
    // scalars shift them, so `at` can land mid-character. Clamp to a boundary — the
    // check is a text heuristic anyway.
    let mut at = at.min(text.len());
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    let (start, end) = match text[..at].rfind("{{") {
        Some(open) if !text[open..at].contains("}}") => {
            (open, text[at..].find("}}").map_or(text.len(), |c| at + c))
        }
        _ => (
            text[..at].rfind('\n').map_or(0, |i| i + 1),
            text[at..].find('\n').map_or(text.len(), |i| at + i),
        ),
    };
    let expr = &text[start..end];
    expr.contains("default(") || expr.contains("defined")
}

/// [`definitions`] plus the set of files it read (canonicalised) — its dependencies, so a
/// cache can invalidate this result precisely when any of them changes.
pub fn definitions_with_deps(path: &Path, nodes: &[Node]) -> (Vec<Located>, HashSet<PathBuf>) {
    let mut out = Vec::new();
    // `visited` doubles as the dependency set: every file the walk reads is recorded here.
    let mut visited = HashSet::new();
    if let Ok(c) = path.canonicalize() {
        visited.insert(c);
    }
    collect(path, nodes, 0, &mut out, &mut visited);
    // Same var reached by two paths (e.g. a vars file two plays share) collapses.
    let mut seen = HashSet::new();
    out.retain(|d| seen.insert((d.name.clone(), d.file.clone(), d.span.start)));
    (out, visited)
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
        read_var_file(&role.join("defaults").join("main.yml"), VarSource::RoleDefaults, None, visited, out);
        read_var_file(&role.join("vars").join("main.yml"), VarSource::RoleVars, None, visited, out);

        // meta/main.yml dependencies run before this role, so their defaults/vars and
        // set_facts are in scope here — and, transitively, for whoever calls this role
        // (entering a dependency's files rediscovers *its* role context and deps).
        if depth < MAX_DEPTH {
            let meta = role.join("meta").join("main.yml");
            if let Ok(text) = std::fs::read_to_string(&meta) {
                if let Some(mnodes) = Document::new(text).parse() {
                    let mctx = FileContext::discover(&meta);
                    for dep in references::meta_dependencies(&mnodes) {
                        for target in resolve::resolve(&dep, &mctx).targets {
                            for f in role_task_files(&target) {
                                collect_disk(&f, depth + 1, out, visited);
                            }
                        }
                    }
                }
            }
        }
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
                        read_var_file(&f, VarSource::VarsFiles, None, visited, out);
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
        let Some(params) = include_vars::params_from_args(&a.args) else { return };
        // The dir form runs the ported plugin semantics — one computed root, the real
        // filters — and indexes exactly the files Ansible would load, in load order.
        let dir = params.dir.clone().or_else(|| {
            params.raw_params.as_deref().and_then(|raw| {
                crate::splitter::parse_kv(raw, false)
                    .ok()
                    .and_then(|kv| kv.get("dir").map(str::to_string))
            })
        });
        if let Some(dir) = dir {
            if !dir.contains("{{") {
                let ictx = include_vars::Ctx {
                    role_path: ctx.role_dir.as_deref(),
                    task_dir: &ctx.file_dir,
                };
                if let include_vars::Outcome::Loaded(l) =
                    include_vars::load(&params, &ictx, &include_vars::StdFs)
                {
                    for f in &l.files {
                        read_var_file(f, VarSource::IncludeVars, cond.clone(), visited, out);
                    }
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
                    read_var_file(&path, VarSource::IncludeVars, cond, visited, out);
                }
            }
        }
    });

    // Playbook-adjacent group_vars/ and host_vars/ — a fixed location next to this file, not
    // a workspace scan. The inventory-adjacent copies (next to a separate inventory file)
    // need the inventory's location, which we don't guess, so those stay unindexed.
    read_var_dir(&ctx.file_dir.join("group_vars"), true, visited, out);
    read_var_dir(&ctx.file_dir.join("host_vars"), false, visited, out);

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
fn read_var_dir(dir: &Path, group: bool, visited: &mut HashSet<PathBuf>, out: &mut Vec<Located>) {
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
        read_var_file(&p, source, None, visited, out);
    }
}

/// Read a flat `name: value` vars file and index every top-level key. `condition` is the
/// `when:` guarding the load, if any (only `include_vars`, a task, can carry one). Records
/// the file (canonicalised) in `visited` as a dependency.
fn read_var_file(
    file: &Path,
    source: VarSource,
    condition: Option<String>,
    visited: &mut HashSet<PathBuf>,
    out: &mut Vec<Located>,
) {
    let Ok(text) = std::fs::read_to_string(file) else {
        return;
    };
    if let Ok(c) = file.canonicalize() {
        visited.insert(c);
    }
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

    /// Names `undefined_uses` flags for a source, with cross-file reads landing nowhere.
    fn undef(src: &str) -> Vec<String> {
        let nodes = Document::new(src.to_string()).parse().expect("valid yaml");
        let path = Path::new("/nonexistent-t051/play.yml");
        let mut names: Vec<String> = undefined_uses(path, &nodes, src)
            .into_iter()
            .map(|u| u.name)
            .collect();
        names.dedup();
        names
    }

    #[test]
    fn undefined_use_in_a_playbook_is_flagged() {
        // The demo's region case: templated include_vars path, nothing defines the var.
        let src = "- hosts: all\n  tasks:\n    - include_vars: \"vars/{{ region }}.yml\"\n";
        assert_eq!(undef(src), ["region"]);
    }

    #[test]
    fn any_reachable_definition_exempts_even_a_later_one() {
        let src = concat!(
            "- hosts: all\n  tasks:\n",
            "    - debug: { msg: \"{{ later }}\" }\n",
            "    - set_fact: { later: 1 }\n",
        );
        assert!(undef(src).is_empty());
    }

    #[test]
    fn magic_facts_and_loop_names_stay_silent() {
        let src = concat!(
            "- hosts: all\n  tasks:\n",
            "    - debug: { msg: \"{{ playbook_dir }} {{ ansible_os_family }} {{ item }}\" }\n",
            "      loop: [1]\n",
            "    - debug: { msg: \"{{ my_row }}\" }\n",
            "      loop: [1]\n",
            "      loop_control: { loop_var: my_row }\n",
        );
        assert!(undef(src).is_empty());
    }

    #[test]
    fn handled_undefinedness_stays_silent() {
        let src = concat!(
            "- hosts: all\n  tasks:\n",
            "    - debug: { msg: \"{{ maybe | default('x') }}\" }\n",
            "    - debug: { msg: \"{{ gated }}\" }\n",
            "      when: gated is defined\n",
        );
        assert!(undef(src).is_empty());
    }

    #[test]
    fn vars_prompt_and_jinja_set_names_stay_silent() {
        let src = concat!(
            "- hosts: all\n",
            "  vars_prompt:\n",
            "    - name: password\n",
            "  tasks:\n",
            "    - debug: { msg: \"{{ password }} {% set tmp = 1 %}{{ tmp }}\" }\n",
        );
        assert!(undef(src).is_empty());
    }

    /// A tasks/role file can receive vars from any caller — never diagnosable.
    #[test]
    fn task_files_are_never_flagged() {
        assert!(undef("- debug: { msg: \"{{ from_caller }}\" }\n").is_empty());
    }

    /// A meta/main.yml dependency runs first, so its defaults and set_facts are in scope
    /// for the depending role AND for the playbook that calls it — transitively.
    #[test]
    fn meta_dependency_vars_reach_the_role_and_its_caller() {
        let d = std::env::temp_dir().join("ansible-lsp-t018-scope");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write(&d, "roles/base/defaults/main.yml", "base_mtu: 1500\n");
        write(&d, "roles/base/tasks/main.yml", "- set_fact:\n    base_up: true\n");
        write(&d, "roles/app/meta/main.yml", "dependencies:\n  - base\n");
        write(&d, "roles/app/tasks/main.yml", "- debug: { msg: \"{{ base_mtu }}\" }\n");
        let play = d.join("play.yml");
        std::fs::write(&play, "- hosts: all\n  roles: [app]\n").unwrap();

        // From inside the depending role's own tasks file:
        let tasks = d.join("roles/app/tasks/main.yml");
        let nodes = Document::new(std::fs::read_to_string(&tasks).unwrap()).parse().unwrap();
        let defs = definitions(&tasks, &nodes);
        assert!(defs.iter().any(|x| x.name == "base_mtu"));

        // And from the playbook that calls the role — two hops away:
        let nodes = Document::new(std::fs::read_to_string(&play).unwrap()).parse().unwrap();
        let defs = definitions(&play, &nodes);
        assert!(defs.iter().any(|x| x.name == "base_mtu"));
        assert!(defs.iter().any(|x| x.name == "base_up"));
    }

    /// A dependency cycle (a <-> b) must terminate — the walk's visited set is the guard,
    /// and both sides' vars still land. Termination IS the assertion: without the guard
    /// this test would recurse forever, not fail.
    #[test]
    fn mutual_meta_dependencies_terminate_and_both_contribute() {
        let d = std::env::temp_dir().join("ansible-lsp-t018-cycle");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write(&d, "roles/a/meta/main.yml", "dependencies:\n  - b\n");
        write(&d, "roles/b/meta/main.yml", "dependencies:\n  - a\n");
        write(&d, "roles/a/defaults/main.yml", "a_var: 1\n");
        write(&d, "roles/b/defaults/main.yml", "b_var: 2\n");
        write(&d, "roles/a/tasks/main.yml", "- debug: { msg: hi }\n");
        write(&d, "roles/b/tasks/main.yml", "- debug: { msg: hi }\n");
        let play = d.join("play.yml");
        std::fs::write(&play, "- hosts: all\n  roles: [a]\n").unwrap();

        let nodes = Document::new(std::fs::read_to_string(&play).unwrap()).parse().unwrap();
        let defs = definitions(&play, &nodes);
        assert!(defs.iter().any(|x| x.name == "a_var"));
        assert!(defs.iter().any(|x| x.name == "b_var"));
    }

    /// Block scalars shift value-relative spans off source char boundaries — the
    /// corpus file that panicked the first gate run, minimised.
    #[test]
    fn undefined_check_survives_misaligned_spans_in_block_scalars() {
        let src = "- hosts: all\n  tasks:\n    - debug:\n        msg: |\n          aa\n          ═══{{ zzz_und }}\n";
        let _ = undef(src); // must not panic; the verdict is not the point
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
    fn deps_include_the_files_the_walk_read() {
        let d = std::env::temp_dir().join("ansible-lsp-deps");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write(&d, "vars/shared.yml", "x: 1\n");
        write(&d, "roles/r/tasks/main.yml", "- set_fact: { y: 2 }\n");
        let play = d.join("play.yml");
        std::fs::write(
            &play,
            "- hosts: all\n  vars_files: [vars/shared.yml]\n  roles: [r]\n",
        )
        .unwrap();
        let nodes = Document::new(std::fs::read_to_string(&play).unwrap())
            .parse()
            .unwrap();
        let (_defs, deps) = definitions_with_deps(&play, &nodes);
        // A change to any of these must be able to invalidate this file's cached result.
        let has = |rel: &str| {
            let want = d.join(rel).canonicalize().unwrap();
            deps.iter().any(|p| *p == want)
        };
        assert!(has("play.yml"), "the root itself");
        assert!(has("vars/shared.yml"), "the vars_files target");
        assert!(has("roles/r/tasks/main.yml"), "the included role task file");
    }

    #[test]
    fn known_literals_takes_value_span_sources_only() {
        let d = std::env::temp_dir().join("ansible-lsp-lits");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write(&d, "host_vars/web01.yml", "region: eu\n");
        let play = d.join("play.yml");
        let src = concat!(
            "- hosts: all\n",
            "  vars:\n",
            "    env: prod\n",
            "  tasks:\n",
            "    - set_fact: { built: yes }\n",
        );
        std::fs::write(&play, src).unwrap();
        let nodes = Document::new(std::fs::read_to_string(&play).unwrap())
            .parse()
            .unwrap();
        let defs = definitions(&play, &nodes);
        let lits = known_literals(&defs, &play, src);
        assert_eq!(lits.get("env"), Some(&vec!["prod".to_string()])); // play var value
        assert!(!lits.contains_key("built")); // set_fact span is the name, excluded
        assert!(!lits.contains_key("region")); // host_vars is host-dependent, excluded
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
