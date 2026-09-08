//! AST -> cross-file references.

use crate::ast::{self, Action, Ast, Import, Play, PlayItem, Stmt, Task};
use crate::parse::{Node, Span};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceKind {
    IncludeTasks,
    ImportTasks,
    /// A role name, from `include_role`/`import_role` or a `roles:` entry.
    Role,
    /// `tasks_from:` — resolves inside whichever role `role` names.
    TasksFrom,
    /// A 3-part FQCN used as a task key, e.g. `community.lvm.pool_create:`.
    Module,
    /// `import_playbook:` — play-level, static, so never legitimately templated.
    ImportPlaybook,
    /// `include_vars:` file target — a vars file that should exist.
    IncludeVars,
    /// `include_vars:` `dir:` target — a directory, resolved by `_set_root_dir` to one
    /// computed path; navigation targets are the files it loads.
    IncludeVarsDir,
    /// `template:` `src:` — the local `.j2` a task renders.
    ///
    /// **Only `template:`.** `src:` means a different thing per module and T-015 owns the
    /// full table: `slurp`, `fetch` and `file` take a path on the *managed host*, where
    /// checking for a local file would warn about correct code. Anything not named here
    /// produces no reference at all, which costs navigation and never invents a warning.
    ///
    /// This kind exists for T-040 — a template's include targets, delimiters and candidate
    /// resolutions are all properties of the task that renders it, and nothing else links the
    /// two. It is deliberately **not diagnosed**: the missing-file verdict on `src:` is
    /// T-015's, behind its corpus gate.
    TemplateSrc,
    /// A play-level `vars_files:` entry — a vars file loaded at play start. A nested list
    /// is first-match-wins: each alternative is its own reference (`grouped`), plus one
    /// group reference spanning the list that owns the missing/resolved verdict.
    VarsFiles,
}

impl ReferenceKind {
    /// The snake_case name `scan` prints and `ansible/whoReferences` returns.
    pub fn name(self) -> &'static str {
        match self {
            ReferenceKind::IncludeTasks => "include_tasks",
            ReferenceKind::ImportTasks => "import_tasks",
            ReferenceKind::Role => "role",
            ReferenceKind::TasksFrom => "tasks_from",
            ReferenceKind::Module => "module",
            ReferenceKind::ImportPlaybook => "import_playbook",
            ReferenceKind::IncludeVars => "include_vars",
            ReferenceKind::IncludeVarsDir => "include_vars_dir",
            ReferenceKind::VarsFiles => "vars_files",
            ReferenceKind::TemplateSrc => "template_src",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Reference {
    pub kind: ReferenceKind,
    pub value: String,
    /// Jinja expression present, so the target is only knowable at runtime.
    pub templated: bool,
    pub span: Span,
    /// For `TemplateSrc`: the delimiters this task passes as module parameters
    /// (`plugins/action/template.py:125-134`). `None` when it passes none, which is almost
    /// always — boxed so the common case costs a pointer rather than eight `String`s.
    ///
    /// A `#jinja2:` header in the template itself **beats** these for the fields it names;
    /// measured on 2.21.2, where a header's `[% %]` won over a task passing
    /// `variable_start_string: "<<"`.
    pub template_delimiters: Option<Box<crate::jinja::Delimiters>>,
    /// For `TasksFrom`: the role it belongs to, read from the same mapping node.
    pub role: Option<String>,
    /// For `Role`: the same include carried a `tasks_from`, so `tasks/main.yml` is
    /// not required — a role can exist purely as named task files.
    pub has_tasks_from: bool,
    /// The containing task has a `when:`, so this call may not happen.
    pub conditional: bool,
    /// The `when:` clauses themselves, for [`crate::condition`]. A list `when:` is
    /// several clauses ANDed together.
    pub conditions: Vec<String>,
    /// Span of the `when:` value, so a problem with the condition is reported on the
    /// condition rather than on the reference a few lines away.
    pub condition_span: Option<Span>,
    /// Span of the `when:` keyword itself. The value belongs to the variables written in
    /// it; the keyword is the one token that means "this guard" and nothing else, so it
    /// is what an explanation of the guard anchors on (T-078).
    pub condition_key_span: Option<Span>,
    /// This reference's `when:` is copied onto every task it brings in and re-evaluated
    /// per task, rather than gating the reference once. True for the static forms —
    /// `import_playbook`, `import_tasks`, `import_role`, and a `roles:` entry. False for a
    /// dynamic `include_*`, whose `when:` decides once whether to include at all.
    ///
    /// The difference is what makes `when-import-var-mutated` possible: when the condition
    /// is re-evaluated per task, a variable the target itself assigns can flip it partway
    /// through and the file half-executes. Measured on 2.21.2 for all five forms (T-166).
    /// `include_role`/`import_role` share one [`ReferenceKind`], so the kind cannot answer
    /// this — it is recorded where the action name is still in hand.
    pub when_propagates: bool,
    /// A `when:` written inside a dynamic include's `apply:`. Separate from
    /// [`conditions`](Reference::conditions) because the two are independent gates that
    /// both have to pass — measured: own `when: true` with `apply: {when: false}` skips the
    /// included task, and so does the reverse. The task's own `when:` decides whether the
    /// include happens at all; this one is copied onto each task it brings in, which is
    /// what makes it the second half of `when_propagates` (T-166).
    pub apply_when: Vec<String>,
    pub apply_when_span: Option<Span>,
    /// `vars:` written inside a dynamic include's `apply:`, as (name, value span) in
    /// written order. They bind on the tasks the include brings in and **not** on the
    /// including file — measured on 2.21.2: a task after the include reads the name as
    /// undefined, while the included file and a further include nested inside it both see
    /// it (T-167). Same Block mechanism as [`Reference::apply_when`], so the precedence is
    /// block vars' (16): measured to beat a role's `vars/main.yml` and to lose to a task
    /// `vars:`, `include_vars`, `set_fact` and an include param of the same name.
    pub apply_vars: Vec<(String, Span)>,
    /// Span of the whole `apply:` value, which is the range in *this* file those vars are
    /// confined to.
    pub apply_span: Option<Span>,
    /// The containing task has a `loop:`/`with_*`, so it may happen many times.
    pub repeated: bool,
    /// The containing task's `name:`, for labelling an execution tree.
    pub task_name: Option<String>,
    /// For the `IncludeVarsDir` kind: the full module args, so the resolver can run the
    /// real loading semantics (extensions, depth, filters) instead of re-parsing.
    pub include_vars: Option<Box<crate::include_vars::Params>>,
    /// A first-match `vars_files` alternative: a miss is the construct working as
    /// designed, so it never warns on its own — the group reference decides.
    pub grouped: bool,
    /// The group reference of a first-match `vars_files` list: every alternative, in
    /// written order, so the group can resolve first-found and name them all when none
    /// exists. `span` is the whole nested list.
    pub vars_files_group: Option<Vec<String>>,
    /// Literal `vars:` written on an `import_playbook` entry. Substituted into the value
    /// before anything else, because at parse time it is one of only two sources Ansible
    /// can read (the other, `-e`, is invisible to us) — T-095.
    pub entry_vars: Vec<(String, String)>,
    /// A genuine playbook-level `import_playbook:` entry, not the same key written inside a
    /// task list. Ansible loads only the first as a playbook; the second is read as a module
    /// name and fails on its parameters (T-110 row `ip`), so its target is never opened and
    /// has nothing to judge.
    pub playbook_entry: bool,
}

impl Reference {
    /// The condition that lands on **every task this reference brings in**, and is
    /// therefore re-evaluated per task — the thing a variable the target itself assigns can
    /// flip partway through. `None` when the reference gates once, or not at all.
    ///
    /// The one question `when-import-var-mutated` asks. Keeping it here rather than at the
    /// two call sites is what let the rule stop hard-coding `kind == ImportPlaybook`.
    pub fn propagated_condition(&self) -> Option<(&[String], Span)> {
        if !self.apply_when.is_empty() {
            return Some((&self.apply_when, self.apply_when_span?));
        }
        if self.when_propagates && !self.conditions.is_empty() {
            return Some((&self.conditions, self.condition_span?));
        }
        None
    }

    pub(crate) fn new(kind: ReferenceKind, value: &str, span: Span) -> Self {
        Self {
            kind,
            value: value.to_string(),
            templated: value.contains("{{"),
            span,
            template_delimiters: None,
            role: None,
            has_tasks_from: false,
            conditional: false,
            conditions: Vec::new(),
            condition_span: None,
            condition_key_span: None,
            repeated: false,
            task_name: None,
            include_vars: None,
            grouped: false,
            vars_files_group: None,
            entry_vars: Vec::new(),
            when_propagates: false,
            apply_when: Vec::new(),
            apply_when_span: None,
            apply_vars: Vec::new(),
            apply_span: None,
            playbook_entry: false,
        }
    }
}

/// `roles/<x>/meta/main.yml`: each `dependencies:` entry names a role that runs before
/// this one, so each is a Role reference. Callers gate by path — a `dependencies:` key in
/// a random vars file is data, not dependencies (`roles/sync-state/vars/main.yml` has one).
pub fn meta_dependencies(nodes: &[Node]) -> Vec<Reference> {
    let mut out = Vec::new();
    for n in nodes {
        for item in n.get("dependencies").map(Node::items).unwrap_or_default() {
            let target = match item {
                Node::Scalar { .. } => Some(item),
                Node::Mapping { .. } => item.get("role").or_else(|| item.get("name")),
                _ => None,
            };
            if let Some(Node::Scalar { value, span }) = target {
                out.push(Reference::new(ReferenceKind::Role, value, *span));
            }
        }
    }
    out
}

/// What [`extract`] found: the references, plus the one fact about the **file** that the
/// resolver needs and a lone reference cannot carry.
pub struct Extracted {
    pub refs: Vec<Reference>,
    /// The file is a playbook, not a task/handler file. It decides what
    /// `{{ playbook_dir }}` is: in a playbook it is that file's own directory, exactly;
    /// elsewhere it is the invoking playbook's, which the file cannot know (T-137).
    ///
    /// It lived on every [`Reference`] until T-135 — N identical bools per file — because
    /// the resolver's unit of work was one reference and there was nowhere else to put it.
    /// It now rides [`crate::resolve::Resolver`], built once per file.
    pub in_playbook: bool,
}

/// Every cross-file reference in a parsed file. Walks the semantic model
/// ([`crate::ast`]) rather than the raw tree, so a task's module and its `when:`/`loop:`
/// context are read from structure instead of re-detected key by key.
pub fn extract(nodes: &[Node]) -> Extracted {
    let mut refs = Vec::new();
    let in_playbook = match ast::build(nodes) {
        Ast::Playbook(items) => {
            items.iter().for_each(|it| play_item(it, &mut refs));
            true
        }
        Ast::Tasks(stmts) => {
            stmts.iter().for_each(|s| stmt(s, &mut refs));
            false
        }
        Ast::Other => false,
    };
    Extracted { refs, in_playbook }
}

fn play_item(item: &PlayItem, out: &mut Vec<Reference>) {
    match item {
        PlayItem::Play(p) => play(p, out),
        PlayItem::Import(i) => import_playbook(i, out),
    }
}

fn play(p: &Play, out: &mut Vec<Reference>) {
    for role in &p.roles {
        let mut r = Reference::new(ReferenceKind::Role, &role.name, role.span);
        // A `roles:` entry inherits the play's identity, not a task's.
        r.task_name = p.name.clone();
        r.conditional = role.when_span.is_some();
        r.conditions = role.when.clone();
        r.condition_span = role.when_span;
        r.when_propagates = true;
        out.push(r);
    }
    for entry in &p.vars_files {
        // Alternatives first: `reference_at` takes the first span hit, so a click inside
        // an alternative must reach it before the enclosing group span.
        for (value, span) in &entry.alternatives {
            let mut r = Reference::new(ReferenceKind::VarsFiles, value, *span);
            r.grouped = entry.alternatives.len() > 1;
            r.task_name = p.name.clone();
            out.push(r);
        }
        if entry.alternatives.len() > 1 {
            let names: Vec<String> = entry.alternatives.iter().map(|(v, _)| v.clone()).collect();
            let mut g = Reference::new(ReferenceKind::VarsFiles, &names.join(", "), entry.span);
            g.task_name = p.name.clone();
            g.vars_files_group = Some(names);
            out.push(g);
        }
    }
    for s in p
        .pre_tasks
        .iter()
        .chain(&p.tasks)
        .chain(&p.post_tasks)
        .chain(&p.handlers)
    {
        stmt(s, out);
    }
}

/// `when` is a directive on tasks, blocks and plays alike, so its keyword span is already
/// collected — nothing needs to change in the AST to anchor on it.
fn when_key_span(directives: &[ast::Directive]) -> Option<Span> {
    directives.iter().find(|d| d.key == "when").map(|d| d.key_span)
}

fn import_playbook(i: &Import, out: &mut Vec<Reference>) {
    if let Some(file) = &i.file {
        let mut r = Reference::new(ReferenceKind::ImportPlaybook, file, i.span);
        // Reached from `PlayItem::Import`, which only a playbook-level entry produces.
        r.playbook_entry = true;
        // A `when:` on a static import isn't a gate — it's copied onto every imported task.
        r.conditional = i.when_span.is_some();
        r.conditions = i.when.clone();
        r.condition_span = i.when_span;
        r.condition_key_span = when_key_span(&i.directives);
        r.when_propagates = true;
        r.entry_vars = i.vars.clone();
        out.push(r);
    }
}

fn stmt(s: &Stmt, out: &mut Vec<Reference>) {
    match s {
        Stmt::Task(t) => task(t, out),
        // A block-level `when:` propagates to each contained task at runtime, but that's
        // the resolver's concern; here a block only nests statements.
        Stmt::Block(b) => b
            .block
            .iter()
            .chain(&b.rescue)
            .chain(&b.always)
            .for_each(|s| stmt(s, out)),
    }
}

/// A `when:` value is one expression or a list of them (ANDed) — the same shape
/// [`crate::ast`] reads, needed here because `apply:` is raw args, not a built `Task`.
fn clauses_of(when: &Node) -> Vec<String> {
    match when {
        Node::Sequence { items, .. } => {
            items.iter().filter_map(|i| i.as_str().map(str::to_owned)).collect()
        }
        other => other.as_str().map(str::to_owned).into_iter().collect(),
    }
}

fn task(t: &Task, out: &mut Vec<Reference>) {
    let Some(action) = &t.action else { return };
    let before = out.len();
    module_refs(action, out);
    let key_span = when_key_span(&t.directives);
    // Only the *static* forms copy the task's `when:` onto what they bring in. A dynamic
    // `include_*` evaluates it once, deciding whether to include at all — measured: an
    // `include_tasks` gated on a variable its target assigns skips nothing (T-166).
    let propagates = matches!(
        crate::keywords::core_action(&action.name),
        "import_tasks" | "import_role"
    );
    // A dynamic include's `apply:` is a Block wrapping what it brings in, so a `when:`
    // inside it is inherited by every one of those tasks, and its `vars:` bind on them
    // (T-167). Only the mapping spelling has one.
    //
    // Read **only** on the dynamic forms. `apply` on an import is not merely unsupported,
    // it stops the run: measured on 2.21.2, both `import_tasks` and `import_role` die with
    // "Invalid options for import_tasks: apply" before any task executes. An earlier
    // comment here asserted such a task "never reaches here" — it does, measured on our own
    // extractor, which handed an `ImportTasks` reference the entries. Reading them would
    // have us answer a hover for a name that never binds, on a playbook that never runs.
    let apply = matches!(crate::keywords::core_action(&action.name), "include_tasks" | "include_role")
        .then(|| action.args.get("apply"))
        .flatten();
    let apply_when = apply.and_then(|a| a.get("when")).map(|w| (clauses_of(w), w.span()));
    // The same Block's `vars:`, which bind on what the include brings in (T-167).
    let apply_vars: Vec<(String, Span)> = apply
        .and_then(|a| a.get("vars"))
        .map(|v| {
            v.entries()
                .iter()
                .filter_map(|(k, val)| Some((k.as_str()?.to_owned(), val.span())))
                .collect()
        })
        .unwrap_or_default();
    // The task's `when:`/`loop:`/`name:` belong to every reference it produced.
    for r in &mut out[before..] {
        r.conditional = t.when_span.is_some();
        r.conditions = t.when.clone();
        r.condition_span = t.when_span;
        r.condition_key_span = key_span;
        r.when_propagates = propagates;
        if let Some((cl, sp)) = &apply_when {
            r.apply_when = cl.clone();
            r.apply_when_span = Some(*sp);
        }
        if !apply_vars.is_empty() {
            r.apply_vars = apply_vars.clone();
            r.apply_span = apply.map(Node::span);
        }
        r.repeated = t.looped;
        r.task_name = t.name.clone();
    }
}

/// The reference(s) a task's module implies: an include target, a role + `tasks_from`, or
/// a bare FQCN module.
/// The six delimiter parameters the `template:` module takes
/// (`plugins/action/template.py:125-134`). `None` when the task passes none of them, so a
/// caller can tell "no override" from "override to the defaults" without comparing eight
/// strings.
///
/// The other three template-only parameters — `trim_blocks`, `lstrip_blocks` and
/// `newline_sequence` — are read by ansible and deliberately not by us: measured over 20,010
/// templates, they change the rendered bytes and never the parse verdict or the reference set
/// (see `jinja::template`).
fn template_delimiters(args: &Node) -> Option<crate::jinja::Delimiters> {
    let get = |k: &str| match args.get(k) {
        Some(Node::Scalar { value, .. }) if !value.contains("{{") => Some(value.clone()),
        _ => None,
    };
    let fields = [
        ("block_start_string", 0),
        ("block_end_string", 1),
        ("variable_start_string", 2),
        ("variable_end_string", 3),
        ("comment_start_string", 4),
        ("comment_end_string", 5),
    ];
    let read: Vec<Option<String>> = fields.iter().map(|(k, _)| get(k)).collect();
    if read.iter().all(Option::is_none) {
        return None;
    }
    let mut d = crate::jinja::Delimiters::default();
    if let Some(v) = read[0].clone() { d.block_start = v; }
    if let Some(v) = read[1].clone() { d.block_end = v; }
    if let Some(v) = read[2].clone() { d.variable_start = v; }
    if let Some(v) = read[3].clone() { d.variable_end = v; }
    if let Some(v) = read[4].clone() { d.comment_start = v; }
    if let Some(v) = read[5].clone() { d.comment_end = v; }
    Some(d)
}

fn module_refs(a: &Action, out: &mut Vec<Reference>) {
    match crate::keywords::core_action(&a.name) {
        "include_tasks" | "import_tasks" => {
            let kind = if crate::keywords::core_action(&a.name) == "include_tasks" {
                ReferenceKind::IncludeTasks
            } else {
                ReferenceKind::ImportTasks
            };
            // `include_tasks: f.yml` and `include_tasks:\n  file: f.yml`
            let target = match &a.args {
                Node::Scalar { .. } => Some(&a.args),
                Node::Mapping { .. } => a.args.get("file"),
                _ => None,
            };
            if let Some(Node::Scalar { value, span }) = target {
                out.push(Reference::new(kind, value, *span));
            }
        }

        // `import_playbook` as a task key is unusual, but keep parity with the old walk.
        "import_playbook" => {
            if let Node::Scalar { value, span } = &a.args {
                out.push(Reference::new(ReferenceKind::ImportPlaybook, value, *span));
            }
        }

        "include_role" | "import_role" => {
            let name = match a.args.get("name") {
                Some(Node::Scalar { value, span }) => Some((value.clone(), *span)),
                _ => None,
            };
            let tasks_from = a.args.get("tasks_from");
            if let Some((n, span)) = &name {
                let mut r = Reference::new(ReferenceKind::Role, n, *span);
                r.has_tasks_from = tasks_from.is_some();
                out.push(r);
            }
            if let Some(Node::Scalar { value: from, span }) = tasks_from {
                let mut r = Reference::new(ReferenceKind::TasksFrom, from, *span);
                r.role = name.map(|(n, _)| n);
                out.push(r);
            }
        }

        // T-040's link, not T-015's feature. See `ReferenceKind::TemplateSrc`.
        "template" => {
            let src = match &a.args {
                // The k=v spelling: `template: src=x.j2 dest=/etc/x`.
                Node::Scalar { value, span } => crate::splitter::parse_kv(value, false)
                    .ok()
                    .and_then(|kv| kv.get("src").map(|s| (s.to_string(), *span))),
                Node::Mapping { .. } => match a.args.get("src") {
                    Some(Node::Scalar { value, span }) => Some((value.clone(), *span)),
                    _ => None,
                },
                _ => None,
            };
            if let Some((value, span)) = src {
                let mut r = Reference::new(ReferenceKind::TemplateSrc, &value, span);
                r.template_delimiters = template_delimiters(&a.args).map(Box::new);
                out.push(r);
            }
        }

        "include_vars" => {
            match &a.args {
                // The bare scalar is a k=v line, not a path: `=` tokens are options
                // (`dir=` even flips the kind), the non-`=` remainder is the file.
                Node::Scalar { value, span } => {
                    match crate::splitter::parse_kv(value, false) {
                        Ok(kv) => {
                            if let Some(d) = kv.get("dir") {
                                let mut r =
                                    Reference::new(ReferenceKind::IncludeVarsDir, d, *span);
                                r.include_vars =
                                    crate::include_vars::params_from_args(&a.args).map(Box::new);
                                out.push(r);
                            } else if let Some(f) = kv.get("file").or(kv.raw_params.as_deref()) {
                                out.push(Reference::new(ReferenceKind::IncludeVars, f, *span));
                            }
                            // Options only, or a path split by its own `=`: no reference —
                            // that's a provable task failure, the lint's territory.
                        }
                        // Unbalanced quotes/jinja: Ansible fails this at parse time; keep
                        // the old whole-value reference rather than dropping it silently.
                        Err(_) => {
                            out.push(Reference::new(ReferenceKind::IncludeVars, value, *span))
                        }
                    }
                }
                Node::Mapping { .. } => {
                    if let Some(Node::Scalar { value: d, span }) = a.args.get("dir") {
                        let mut r = Reference::new(ReferenceKind::IncludeVarsDir, d, *span);
                        r.include_vars =
                            crate::include_vars::params_from_args(&a.args).map(Box::new);
                        out.push(r);
                    } else if let Some(Node::Scalar { value, span }) = a.args.get("file") {
                        out.push(Reference::new(ReferenceKind::IncludeVars, value, *span));
                    }
                }
                _ => {}
            }
        }

        _ => {
            // The AST already knows this key is the module. A 3-part dotted name is a
            // collection FQCN; a bare name is implicitly `ansible.legacy.<name>` — valid
            // Ansible, resolved through the legacy search order. 2-part names stay
            // unextracted (never valid; the T-042 ERROR will need them extracted first).
            let dots = a.name.split('.').count();
            if (dots == 1 || dots == 3) && !a.name.contains(' ') && !a.name.contains('{') {
                out.push(Reference::new(ReferenceKind::Module, &a.name, a.key_span));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::Document;

    fn refs(src: &str) -> Vec<Reference> {
        let doc = Document::new(src.to_string());
        extract(&doc.parse().expect("valid yaml")).refs
    }

    fn of(src: &str, kind: ReferenceKind) -> Vec<Reference> {
        refs(src).into_iter().filter(|r| r.kind == kind).collect()
    }

    /// T-166: which constructs copy their `when:` onto what they bring in. This is the
    /// whole eligibility rule for `when-import-var-mutated`, which used to be a hard-coded
    /// `kind == ImportPlaybook` in two places. Every row measured on 2.21.2 against a
    /// target that `set_fact`s the variable its own condition reads: the `true` rows
    /// half-execute, the `false` row skips nothing.
    ///
    /// `include_role` and `import_role` share one `ReferenceKind`, so the kind alone can
    /// never decide this — which is why the flag is set where the action name is known.
    #[test]
    fn only_the_static_forms_propagate_their_when() {
        let propagates = |src: &str| {
            let r = refs(src);
            let hit = r
                .iter()
                .find(|r| matches!(r.kind, ReferenceKind::Role | ReferenceKind::ImportTasks
                    | ReferenceKind::IncludeTasks | ReferenceKind::ImportPlaybook))
                .unwrap_or_else(|| panic!("no reference in {src:?}"));
            (hit.when_propagates, hit.conditions.clone())
        };
        let w = "when: not (done | default(false))";
        // Static: the condition lands on every task the target contributes.
        for src in [
            format!("- import_playbook: p.yml\n  {w}\n"),
            format!("- hosts: all\n  tasks:\n    - import_tasks: t.yml\n      {w}\n"),
            format!("- hosts: all\n  tasks:\n    - import_role: {{name: r}}\n      {w}\n"),
            format!("- hosts: all\n  roles:\n    - role: r\n      {w}\n"),
        ] {
            let (p, c) = propagates(&src);
            assert!(p, "should propagate: {src}");
            assert_eq!(c.len(), 1, "condition must reach the reference: {src}");
        }
        // Dynamic: evaluated once, so nothing inside can flip it.
        for src in [
            format!("- hosts: all\n  tasks:\n    - include_tasks: t.yml\n      {w}\n"),
            format!("- hosts: all\n  tasks:\n    - include_role: {{name: r}}\n      {w}\n"),
        ] {
            let (p, c) = propagates(&src);
            assert!(!p, "must not propagate: {src}");
            assert_eq!(c.len(), 1, "the condition is still recorded, just not propagated");
        }
    }

    /// T-166: a dynamic include's `apply:` is a Block around what it brings in, so a
    /// `when:` inside it *is* re-evaluated per task even though the include's own is not.
    /// The two are independent gates — measured, own `when: true` with
    /// `apply: {when: false}` skips the included task and so does the reverse — so the
    /// apply condition is kept beside the task's rather than replacing it.
    /// `apply:` is include-only, and reading it anywhere else is a wrong answer rather than
    /// a harmless one.
    ///
    /// Measured on 2.21.2: `include_tasks` and `include_role` both carry it, while
    /// `import_tasks` and `import_role` die before any task runs — "Invalid options for
    /// import_tasks: apply". So an import's `apply:` describes a playbook that cannot
    /// execute, and indexing its `vars:` would offer a definition for a name that never
    /// binds. Both halves are asserted here: without the two positive rows this would also
    /// pass on an extractor that read `apply:` from nothing at all.
    #[test]
    fn apply_is_read_on_the_dynamic_includes_and_never_on_an_import() {
        let of_action = |action: &str, arg: &str| {
            let src = format!(
                "- hosts: all\n  tasks:\n    - {action}:\n        {arg}\n        apply:\n          \
                 vars:\n            applied: 1\n          when: go\n"
            );
            let nodes = Document::new(src).parse().expect("fixture parses");
            let refs = extract(&nodes).refs;
            assert!(!refs.is_empty(), "{action} produced a reference at all");
            refs.iter()
                .map(|r| {
                    (
                        r.apply_vars.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(),
                        r.apply_when.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };

        let applied = (vec!["applied".to_string()], vec!["go".to_string()]);
        let none: (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());

        assert_eq!(of_action("include_tasks", "file: inc.yml"), vec![applied.clone()]);
        assert_eq!(of_action("include_role", "name: r"), vec![applied]);
        assert_eq!(of_action("import_tasks", "file: inc.yml"), vec![none.clone()]);
        assert_eq!(of_action("import_role", "name: r"), vec![none]);
    }

    /// A play's `roles:` entry is the third place `apply:` can be written, and it is neither
    /// applied nor rejected there.
    ///
    /// Measured on 2.21.2: the role read `applied_var` as UNDEF while a variable literally
    /// named `apply` held the whole mapping — `{'vars': {'applied_var': 'from_apply'}}`. It
    /// is the T-100 shape, an unknown key in a `roles:` entry silently becoming a role param,
    /// and `apply` is already on T-100's list of keys worth warning about. So the entry must
    /// contribute no apply vars: indexing them would claim a binding the run does not make.
    #[test]
    fn a_roles_entry_apply_binds_nothing() {
        let src = "- hosts: all
  roles:
    - role: re
      apply:
        vars:
          applied: 1
";
        let nodes = Document::new(src.to_string()).parse().expect("fixture parses");
        let refs = extract(&nodes).refs;
        let role: Vec<_> = refs.iter().filter(|r| r.kind == ReferenceKind::Role).collect();
        assert_eq!(role.len(), 1, "the entry still resolves as a role: {refs:#?}");
        assert!(role[0].apply_vars.is_empty(), "and carries no apply vars");
        assert!(role[0].apply_when.is_empty(), "nor an apply when");
    }

    #[test]
    fn an_apply_when_propagates_while_the_includes_own_when_does_not() {
        let src = "- hosts: all\n  tasks:\n    - include_tasks:\n        file: t.yml\n        \
                   apply: {when: not done}\n      when: run_it\n";
        let r = of(src, ReferenceKind::IncludeTasks);
        let r = &r[0];
        // The task's own `when:` is still recorded, and still does not propagate.
        assert_eq!(r.conditions, ["run_it"]);
        assert!(!r.when_propagates);
        // The apply one does, and is what the mutation rule must read.
        assert_eq!(r.apply_when, ["not done"]);
        let (conds, span) = r.propagated_condition().expect("apply when propagates");
        assert_eq!(conds, ["not done"]);
        assert_eq!(span.slice(src), "not done");

        // Without an `apply:`, a dynamic include propagates nothing.
        let plain = of("- hosts: all\n  tasks:\n    - include_tasks: t.yml\n      when: x\n",
                       ReferenceKind::IncludeTasks);
        assert!(plain[0].propagated_condition().is_none());

        // A static import propagates its own — the pre-existing path, unchanged.
        let imp = of("- hosts: all\n  tasks:\n    - import_tasks: t.yml\n      when: x\n",
                     ReferenceKind::ImportTasks);
        assert_eq!(imp[0].propagated_condition().unwrap().0, ["x"]);
    }

    /// T-166 per T-010: `# noqa` is looked up by the *line* of the span the rule reports
    /// on, so every new spelling has to put that span on the line the author would write
    /// the comment. The `apply:` one is the reason this is a test — its condition sits
    /// inside module args, which no other reported condition does, so nothing guaranteed
    /// the lookup still landed on the right line.
    #[test]
    fn noqa_reaches_the_propagated_condition_of_every_spelling() {
        let suppressed = |src: &str| {
            let doc = crate::parse::Document::new(src.to_string());
            let nodes = doc.parse().expect("valid yaml");
            let r = extract(&nodes).refs;
            let hit = r
                .iter()
                .find_map(|r| r.propagated_condition())
                .unwrap_or_else(|| panic!("nothing propagates in {src:?}"));
            doc.is_suppressed(hit.1.start, "when-import-var-mutated")
        };
        let cases = [
            "- import_playbook: p.yml\n  when: not done{}\n",
            "- hosts: all\n  tasks:\n    - import_tasks: t.yml\n      when: not done{}\n",
            "- hosts: all\n  tasks:\n    - import_role: {name: r}\n      when: not done{}\n",
            "- hosts: all\n  roles:\n    - role: r\n      when: not done{}\n",
            "- hosts: all\n  tasks:\n    - include_tasks:\n        file: t.yml\n        \
             apply: {when: not done}{}\n",
        ];
        for c in cases {
            assert!(!suppressed(&c.replace("{}", "")), "unsuppressed: {c}");
            assert!(
                suppressed(&c.replace("{}", " # noqa: when-import-var-mutated")),
                "noqa did not reach it: {c}"
            );
        }
    }

    /// T-166: the demo fixture keeps its promise. Four constructs whose `when:` reaches
    /// what they bring in, and three that look similar and must not — the two GOOD rows
    /// and the suppressed one. A hand-written GOOD/BAD label is a claim, and this is the
    /// assertion behind it.
    #[test]
    fn demo_mutated_conditions_propagates_exactly_the_bad_rows() {
        let path = std::path::Path::new("../../demo/mutated_conditions.yml")
            .canonicalize()
            .unwrap();
        let text = std::fs::read_to_string(&path).expect("demo fixture");
        let doc = crate::parse::Document::new(text.clone());
        let nodes = doc.parse().expect("fixture must parse");
        let propagating: Vec<String> = extract(&nodes).refs
            .iter()
            .filter_map(|r| r.propagated_condition().map(|(_, s)| s))
            .map(|s| text[..s.start].lines().count().to_string())
            .collect();
        // Propagation is structural — it is a property of the construct, not of whether
        // anything is wrong. Six rows propagate: the four BAD ones, the suppressed one
        // (suppression is the reporting layer's job, not the model's), and the GOOD row
        // gated on a variable the target never sets. Only four are *reported*, because
        // the rule additionally needs the target to assign the name — which is why this
        // asserts the structural half and the scan output covers the other.
        assert_eq!(propagating.len(), 6, "{propagating:?}");

        // The two GOOD rows are dynamic includes carrying the same condition against the
        // same target. If either ever starts propagating, the rule has lost its boundary.
        let good = extract(&nodes).refs
            .into_iter()
            .filter(|r| r.kind == ReferenceKind::IncludeTasks && r.apply_when.is_empty())
            .count();
        assert_eq!(good, 1, "the plain include_tasks GOOD row");
        assert!(
            extract(&nodes).refs
                .iter()
                .filter(|r| r.kind == ReferenceKind::IncludeTasks && r.apply_when.is_empty())
                .all(|r| r.propagated_condition().is_none()),
            "a dynamic include's own when: must not propagate"
        );
    }

    #[test]
    fn vars_files_singles_and_bare_string_extract() {
        let src = "- name: web\n  hosts: all\n  vars_files:\n    - vars/a.yml\n    - b.yml\n";
        let r = of(src, ReferenceKind::VarsFiles);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].value, "vars/a.yml");
        assert_eq!(r[0].span.slice(src), "vars/a.yml");
        assert!(!r[0].grouped);
        assert!(r.iter().all(|r| r.vars_files_group.is_none()));
        // A vars_files entry inherits the play's identity, like a roles: entry.
        assert_eq!(r[0].task_name.as_deref(), Some("web"));

        let bare = of("- hosts: all\n  vars_files: a.yml\n", ReferenceKind::VarsFiles);
        assert_eq!(bare.len(), 1);
        assert_eq!(bare[0].value, "a.yml");
    }

    #[test]
    fn vars_files_group_emits_alternatives_then_anchor() {
        let src = "- hosts: all\n  vars_files:\n    - - a.yml\n      - b.yml\n";
        let r = of(src, ReferenceKind::VarsFiles);
        assert_eq!(r.len(), 3);
        assert!(r[0].grouped && r[1].grouped);
        assert!(r[0].vars_files_group.is_none());
        let g = &r[2];
        assert_eq!(g.vars_files_group.as_deref(), Some(&["a.yml".to_string(), "b.yml".to_string()][..]));
        assert_eq!(g.value, "a.yml, b.yml");
        // Anchor spans the whole nested list; alternatives keep their own spans, and they
        // come first so a click inside one never lands on the group.
        assert!(g.span.start <= r[0].span.start && r[1].span.end <= g.span.end);

        // A one-alternative "group" is just a single.
        let single = of("- hosts: all\n  vars_files: [[only.yml]]\n", ReferenceKind::VarsFiles);
        assert_eq!(single.len(), 1);
        assert!(!single[0].grouped);
    }

    #[test]
    fn vars_files_templated_alternative_taints_the_group() {
        let src = "- hosts: all\n  vars_files:\n    - - \"{{ env }}.yml\"\n      - fallback.yml\n";
        let r = of(src, ReferenceKind::VarsFiles);
        let g = r.last().unwrap();
        assert!(g.vars_files_group.is_some());
        assert!(g.templated);
        assert!(!r[1].templated, "the literal alternative itself is not templated");
    }

    #[test]
    fn inline_and_block_forms() {
        let r = of(
            "- include_tasks: a.yml\n- include_tasks:\n    file: b.yml\n",
            ReferenceKind::IncludeTasks,
        );
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].value, "a.yml");
        assert_eq!(r[1].value, "b.yml");
    }

    #[test]
    fn fqcn_normalises_to_the_same_kind() {
        let r = refs("- ansible.builtin.include_tasks: a.yml\n- import_tasks: b.yml\n");
        assert_eq!(r[0].kind, ReferenceKind::IncludeTasks);
        assert_eq!(r[1].kind, ReferenceKind::ImportTasks);
    }

    #[test]
    fn include_vars_file_forms_are_references_dir_form_is_not() {
        let r = of(
            "- include_vars: a.yml\n- include_vars: { file: b.yml }\n- include_vars: { dir: vars }\n",
            ReferenceKind::IncludeVars,
        );
        // Only the two file forms; the dir form is not a file reference.
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].value, "a.yml");
        assert_eq!(r[1].value, "b.yml");
    }

    #[test]
    fn meta_dependencies_both_forms_empty_and_unrelated_keys() {
        let deps = |src: &str| {
            meta_dependencies(&Document::new(src.to_string()).parse().unwrap())
        };
        let r = deps(r#"
            dependencies:
              - docker-network
              - role: podman
                vars: { rootless: true }
              - name: legacy
"#);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].value, "docker-network");
        assert_eq!(r[1].value, "podman");
        assert_eq!(r[2].value, "legacy");
        assert!(r.iter().all(|x| x.kind == ReferenceKind::Role));
        // The 22-roles-of-boilerplate case, and galaxy_info-only files: nothing.
        assert!(deps("dependencies: []\n").is_empty());
        assert!(deps("galaxy_info:\n  author: x\n").is_empty());
    }

    #[test]
    fn include_vars_dir_forms_get_their_own_kind() {
        let r = of(
            "- include_vars: { dir: vars/prod }\n- include_vars: dir=prod name=db\n",
            ReferenceKind::IncludeVarsDir,
        );
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].value, "vars/prod");
        assert_eq!(r[1].value, "prod");
        assert!(r[0].include_vars.is_some());
    }

    #[test]
    fn include_vars_dir_params_are_carried() {
        let r = of(
            "- include_vars:\n    dir: prod\n    extensions: [yml]\n    depth: 1\n",
            ReferenceKind::IncludeVarsDir,
        );
        let p = r[0].include_vars.as_deref().unwrap();
        assert_eq!(p.extensions, ["yml"]);
        assert_eq!(p.depth, 1);
    }

    #[test]
    fn include_vars_free_form_kv_remainder_is_the_file() {
        let r = of("- include_vars: x.yml name=db\n", ReferenceKind::IncludeVars);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].value, "x.yml");
    }

    /// A path split by its own `=` is a provable task failure, not a path reference.
    #[test]
    fn include_vars_options_only_yields_no_reference() {
        assert!(refs("- include_vars: vars/we=ird.yml\n").is_empty());
    }

    #[test]
    fn commented_out_includes_are_not_references() {
        let r = refs("# - include_tasks: ghost.yml\n- include_tasks: real.yml\n");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].value, "real.yml");
    }

    #[test]
    fn templated_values_are_flagged_not_dropped() {
        let r = refs("- include_tasks: \"{{ ap_protocol }}/validate.yml\"\n");
        assert!(r[0].templated);
    }

    #[test]
    fn span_points_at_the_value() {
        let src = "- include_tasks: _converge_one_ap.yml\n";
        assert_eq!(refs(src)[0].span.slice(src), "_converge_one_ap.yml");
    }

    #[test]
    fn task_conditions_attach_to_the_reference() {
        let r = refs(
            "- name: maybe\n  include_tasks: a.yml\n  when: x is defined\n\
             - name: many\n  include_tasks: b.yml\n  loop: [1, 2]\n\
             - name: always\n  include_tasks: c.yml\n",
        );
        assert!(r[0].conditional && !r[0].repeated);
        assert!(r[1].repeated && !r[1].conditional);
        assert!(!r[2].conditional && !r[2].repeated);
        assert_eq!(r[0].task_name.as_deref(), Some("maybe"));
    }

    #[test]
    fn with_items_counts_as_repeated() {
        let r = refs("- include_tasks: a.yml\n  with_items: [1]\n");
        assert!(r[0].repeated);
    }

    #[test]
    fn import_playbook_is_extracted() {
        let r = of(
            "- name: infra\n  import_playbook: lustre-infrastructure.yml\n  when: x\n\
             - ansible.builtin.import_playbook: ../../network-setup.yml\n",
            ReferenceKind::ImportPlaybook,
        );
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].value, "lustre-infrastructure.yml");
        assert_eq!(r[1].value, "../../network-setup.yml");
        // `when:` on a static import is pushed onto every imported task, not a gate.
        assert!(r[0].conditional);
    }

    #[test]
    fn roles_block_bare_and_dict_forms() {
        let r = of(
            "- hosts: all\n  roles:\n    - postgres_setup\n    - role: podman\n",
            ReferenceKind::Role,
        );
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].value, "postgres_setup");
        assert_eq!(r[1].value, "podman");
    }

    /// T-100: the third spelling. `meta_dependencies` already read `name:` as a role name;
    /// a play's `roles:` did not, so this reference did not exist and the name was not
    /// clickable. Live-verified that ansible looks it up.
    #[test]
    fn a_roles_entry_named_with_name_is_a_role_reference() {
        let r = of("- hosts: all\n  roles:\n    - name: podman\n", ReferenceKind::Role);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].value, "podman");
    }

    /// The prototype scans ±6 lines for a sibling `name:`, so the flow form — where
    /// everything is on one line — silently fails.
    #[test]
    fn tasks_from_binds_to_its_own_role_in_flow_form() {
        let r = of(
            "- include_role: { name: cib-batch, tasks_from: begin }\n",
            ReferenceKind::TasksFrom,
        );
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].value, "begin");
        assert_eq!(r[0].role.as_deref(), Some("cib-batch"));
    }

    #[test]
    fn tasks_from_binds_correctly_across_adjacent_tasks() {
        // Two roles in a row: the second tasks_from must not bind to the first name.
        let r = of(
            "- include_role:\n    name: alpha\n    tasks_from: one\n\
             - include_role:\n    name: beta\n    tasks_from: two\n",
            ReferenceKind::TasksFrom,
        );
        assert_eq!(r[0].role.as_deref(), Some("alpha"));
        assert_eq!(r[1].role.as_deref(), Some("beta"));
    }

    #[test]
    fn fqcn_and_bare_module_keys_are_references_but_urls_are_not() {
        // The dotted URL sits in a *value*, never in module-key position, so it can't
        // extract; the bare `debug` key is implicitly ansible.legacy and does.
        let r = of(
            "- community.lvm.pool_create:\n    name: p\n- debug:\n    msg: example.atlassian.net\n",
            ReferenceKind::Module,
        );
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].value, "community.lvm.pool_create");
        assert_eq!(r[1].value, "debug");
    }

    #[test]
    fn only_core_spellings_are_actions() {
        // T-094: Ansible recognises exactly three spellings per action — bare,
        // `ansible.builtin.`, `ansible.legacy.` — so any other dotted key ending in an
        // action's name is an ordinary module in that collection, not the action.
        let cases: &[(&str, &str, ReferenceKind)] = &[
            ("include_tasks", "x.yml", ReferenceKind::IncludeTasks),
            ("import_tasks", "x.yml", ReferenceKind::ImportTasks),
            ("import_playbook", "x.yml", ReferenceKind::ImportPlaybook),
            ("include_role", "{name: r}", ReferenceKind::Role),
            ("import_role", "{name: r}", ReferenceKind::Role),
            ("include_vars", "x.yml", ReferenceKind::IncludeVars),
        ];
        for (action, args, kind) in cases {
            for prefix in ["", "ansible.builtin.", "ansible.legacy."] {
                let src = format!("- {prefix}{action}: {args}\n");
                assert_eq!(of(&src, *kind).len(), 1, "not an action: {src}");
            }
            let fqcn = format!("community.general.{action}");
            let src = format!("- {fqcn}: {args}\n");
            assert!(of(&src, *kind).is_empty(), "treated as an action: {src}");
            let m = of(&src, ReferenceKind::Module);
            assert_eq!(m.len(), 1, "not a module: {src}");
            assert_eq!(m[0].value, fqcn);
        }
    }
}
