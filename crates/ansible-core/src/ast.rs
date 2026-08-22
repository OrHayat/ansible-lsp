//! A semantic model of an Ansible file: plays, blocks, tasks, roles — ordered and nested,
//! each carrying its source [`Span`]. Built on top of the raw [`Node`] tree from
//! [`crate::parse`], using [`crate::keywords`] to tell a task's *module* from its
//! *directives*.
//!
//! This is a parallel layer. The existing resolvers ([`crate::references`],
//! [`crate::condition`]) still walk the raw tree; moving them onto this model is a later
//! step. Nothing here changes current behaviour — it only adds structure the variable and
//! execution features will need.
//!
//! Scope note: the module/args split is best-effort here (the first non-directive key).
//! Ansible's full `ModuleArgsParser` rules — `action:`/`local_action:`, free-form, `args:`
//! — are a follow-up; only the value-naming forms are handled below.

use crate::keywords::{self, KeyContext};
use crate::parse::{Node, Span};

#[derive(Debug, Clone)]
pub enum Ast {
    /// A sequence of plays (and `import_playbook` entries).
    Playbook(Vec<PlayItem>),
    /// A task file: a flat, ordered list of tasks/blocks (`include_tasks` targets, role
    /// `tasks/*.yml`, handler files).
    Tasks(Vec<Stmt>),
    /// Neither — a vars file (top-level mapping), empty, or unrecognised.
    Other,
}

#[derive(Debug, Clone)]
pub enum PlayItem {
    Play(Play),
    /// `- import_playbook: other.yml` — a top-level entry, not a play.
    Import(Import),
}

#[derive(Debug, Clone)]
pub struct Import {
    /// Span of the `import_playbook` value.
    pub span: Span,
    pub file: Option<String>,
    /// Literal `name: value` pairs from a `vars:` written on the import entry itself.
    /// Only scalars — they are the only substitutable form. This is one of exactly two
    /// sources that can supply a templated `import_playbook` (the other is `-e`), because
    /// the import is expanded at parse time from `self.vars | variable_manager.get_vars()`
    /// with no play, host or task (`playbook_include.py:69-83`). T-095.
    pub vars: Vec<(String, String)>,
    /// A `when:` on a static import is copied onto every imported task — see
    /// [`crate::condition`]. ANDed clauses; empty if absent.
    pub when: Vec<String>,
    pub when_span: Option<Span>,
    pub directives: Vec<Directive>,
}

#[derive(Debug, Clone)]
pub struct Play {
    pub span: Span,
    pub name: Option<String>,
    /// Span of the `hosts:` value.
    pub hosts: Option<Span>,
    pub roles: Vec<RoleUse>,
    pub pre_tasks: Vec<Stmt>,
    pub tasks: Vec<Stmt>,
    pub post_tasks: Vec<Stmt>,
    pub handlers: Vec<Stmt>,
    /// `vars:` bound at play scope.
    pub vars: Vec<VarBinding>,
    /// `vars_files:` entries, one per written entry — a nested list stays one entry
    /// with several first-match alternatives.
    pub vars_files: Vec<VarsFilesEntry>,
    /// `vars_files:` items that can never name a file, kept rather than dropped so the
    /// rule can report them (T-087). Recorded here, at the one place that already decides
    /// what a valid entry is, so the diagnostic and the navigation cannot disagree.
    pub invalid_vars_files: Vec<InvalidVarsFilesEntry>,
    /// Play-level directives other than the ones captured structurally above.
    pub directives: Vec<Directive>,
    /// Keys Ansible would reject on this play.
    pub unknown_keys: Vec<UnknownKey>,
}

/// One `vars_files:` entry: a scalar path, or a nested list meaning "load the first of
/// these that exists".
#[derive(Debug, Clone)]
pub struct VarsFilesEntry {
    /// The candidate paths in written order; a scalar entry is one alternative.
    pub alternatives: Vec<(String, Span)>,
    /// Anchor for a whole-entry diagnostic: the scalar's span, or the nested list's span
    /// clamped to the last alternative's end (libyaml's block-sequence end mark can spill
    /// past the last item).
    pub span: Span,
}

/// Why a `vars_files:` item can never name a file. Each arm is the type ansible-core
/// names in the error it dies with — live-verified on 2.21.2, one play per arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidVarsFilesKind {
    /// A `-` with nothing after it, at either level: `type 'NoneType'`.
    Null,
    /// A mapping item, `- dir: x` — the `include_vars` options form, which this keyword
    /// has no equivalent of: `type 'dict'`.
    Mapping,
    /// A list inside the one level of nesting Ansible allows: `type 'list'`. An *empty*
    /// nested list is not this — measured, `- []` runs — so it is dropped, not recorded.
    Nested,
}

/// One `vars_files:` item that fails ansible-core's post-template type gate
/// (`vars/manager.py:348-353`), killing the play before its first task.
#[derive(Debug, Clone)]
pub struct InvalidVarsFilesEntry {
    /// The offending item. A [`InvalidVarsFilesKind::Null`] span is empty and sits where
    /// the value would have been — a caller rendering it wants to widen onto the `-`.
    pub span: Span,
    pub kind: InvalidVarsFilesKind,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Task(Task),
    Block(Block),
}

#[derive(Debug, Clone)]
pub struct Block {
    pub span: Span,
    pub name: Option<String>,
    pub block: Vec<Stmt>,
    pub rescue: Vec<Stmt>,
    pub always: Vec<Stmt>,
    /// `when:` clauses on the block. ANDed; empty if absent.
    pub when: Vec<String>,
    pub when_span: Option<Span>,
    /// `vars:` bound at block scope.
    pub vars: Vec<VarBinding>,
    /// `notify:` written on the block, which every task inside it inherits. Legal here
    /// (`NOTIFIABLE` is in Block's set, `keywords.rs`) — unlike `listen:`, which is not,
    /// and so has no field on this type.
    pub notify: Vec<HandlerRef>,
    pub directives: Vec<Directive>,
    /// Keys Ansible would reject on this block.
    pub unknown_keys: Vec<UnknownKey>,
}

#[derive(Debug, Clone)]
pub struct Task {
    pub span: Span,
    pub name: Option<String>,
    /// The module and its args. `None` for a malformed task with no module key.
    pub action: Option<Action>,
    /// `when:` clauses. ANDed together; empty if the task is unconditional.
    pub when: Vec<String>,
    pub when_span: Option<Span>,
    /// The task has a `loop:`/`with_*`, so it may run many times.
    pub looped: bool,
    /// The literal values a `loop:` list holds, when every item is a plain scalar.
    ///
    /// Empty whenever the iteration is not readable — a `with_*` form, a templated
    /// `loop: "{{ hosts }}"`, or a list with a non-scalar item — which is *not* the same as
    /// an unlooped task: pair it with [`Task::looped`], never read it alone. A consumer that
    /// treats empty as "no iterations" would conclude a templated loop runs zero times.
    pub loop_items: Vec<String>,
    /// `register:` name — a variable this task defines for the rest of the play.
    pub register: Option<String>,
    /// Span of the `register:` value, if present.
    pub register_span: Option<Span>,
    /// `vars:` bound at task scope.
    pub vars: Vec<VarBinding>,
    /// Handler names or `listen:` topics this task notifies.
    pub notify: Vec<HandlerRef>,
    /// `listen:` topics this task subscribes to. Only meaningful in handler position, and
    /// this type does not know whether it is in one — `listen:` on an ordinary task is
    /// already reported as an invalid attribute, so filling the field regardless keeps
    /// that verdict in one place instead of two.
    pub listen: Vec<HandlerRef>,
    pub directives: Vec<Directive>,
    /// Keys Ansible would reject on this task (or under its `loop_control:`).
    pub unknown_keys: Vec<UnknownKey>,
}

#[derive(Debug, Clone)]
pub struct Action {
    /// The module name as written — short (`debug`) or FQCN (`ansible.builtin.debug`).
    pub name: String,
    /// Span of the module key (or of the `action:`/`local_action:` value).
    pub key_span: Span,
    /// The args node: a scalar (`include_tasks: f.yml`) or a mapping (`{ file: f.yml }` /
    /// module parameters). Kept whole so callers can read `file:`/`name:`/`tasks_from:`.
    pub args: Node,
}

#[derive(Debug, Clone)]
pub struct RoleUse {
    pub name: String,
    pub span: Span,
    /// The whole entry, not just the name. Role params and the entry's `vars:` are in
    /// scope *within* it — one param can be built from another (`app_config_dir:
    /// /etc/x/{{ app_env }}`) — and out of scope in the play's own tasks, so a reader
    /// deciding whether a name is defined needs to know which side of this it is on.
    pub entry_span: Span,
    /// Keys of the entry outside `RoleInclude.fattributes`, which ansible-core turns into
    /// variables scoped to this role rather than rejecting (`definition.py:200-224`).
    /// Every one of them is a real variable definition; whether any is *worth reporting*
    /// is [`crate::attributes`]'s call, not this one's. Empty for the bare-string form.
    pub params: Vec<RoleParam>,
    /// The entry's own `vars:` mapping. A legal keyword, unlike [`RoleUse::params`], and
    /// the *documented* way to pass values to a role — so it is the commoner of the two
    /// by a wide margin. Combined after the params (`role/__init__.py:552-558`), so on a
    /// name written both ways this one wins.
    pub vars: Vec<VarBinding>,
    /// `when:` on the entry. Copied onto every task the role contributes and re-evaluated
    /// per task, exactly like a static import's — measured (T-166).
    pub when: Vec<String>,
    pub when_span: Option<Span>,
}

/// A role param: one `key: value` of a `roles:` entry that named no keyword. Carries both
/// spans because the two readers want different ones — the diagnostic underlines the key,
/// the variable index jumps to the value.
#[derive(Debug, Clone)]
pub struct RoleParam {
    pub name: String,
    pub key_span: Span,
    pub value_span: Span,
}

/// One name written in a `notify:` or a `listen:`.
///
/// Carries the value, not just a span, because these are the first references matched by
/// *name* rather than resolved as a path — and one entry per written name, because the
/// list form is several independent references and a single span over the sequence could
/// not anchor a diagnostic on the one that missed.
#[derive(Debug, Clone)]
pub struct HandlerRef {
    pub name: String,
    pub span: Span,
    /// Contains `{{ }}`. The two keys diverge here, measured on 2.21.2: a handler's
    /// `name:` *is* templated, so `notify: restart nginx` reaches
    /// `name: "restart {{ svc }}"` — which makes a templated name a wildcard no rule can
    /// prove absent. `listen:` is *not* templated: the braces stay in the topic, so
    /// notifying the rendered value is a fatal "handler not found" (see
    /// [`crate::static_fields`], which already reports that half).
    pub templated: bool,
}

/// An Ansible-owned key on a play/block/task, with the spans of its key and value.
#[derive(Debug, Clone)]
pub struct Directive {
    pub key: String,
    pub key_span: Span,
    pub value: Span,
}

/// A key ansible-core would reject in its context — `'%s' is not a valid attribute for a
/// %s` (`base.py:211-220`). Classified once here, module keys / `with_*` / `local_action`
/// already excluded, so rules need no keyword knowledge of their own (T-107).
#[derive(Debug, Clone)]
pub struct UnknownKey {
    pub key: String,
    pub key_span: Span,
    /// The context whose legal set rejected the key — names the class in the message.
    pub ctx: KeyContext,
}

/// A `name: value` entry under a `vars:` mapping. The span covers the value, to anchor
/// go-to-definition on the variable.
#[derive(Debug, Clone)]
pub struct VarBinding {
    pub name: String,
    pub span: Span,
}

/// Lift the raw document tree into the semantic model.
pub fn build(nodes: &[Node]) -> Ast {
    // An Ansible file is a single YAML document that is a sequence (multiple `---`
    // documents are rare); operate on the first sequence found.
    let Some(seq) = nodes.iter().find(|n| matches!(n, Node::Sequence { .. })) else {
        return Ast::Other;
    };
    let items = seq.items();
    let looks_like_plays = items
        .iter()
        .any(|it| keywords::is_play(it.entries().iter().filter_map(|(k, _)| k.as_str())));
    if looks_like_plays {
        Ast::Playbook(items.iter().filter_map(build_play_item).collect())
    } else {
        // A standalone task file may be a role's `tasks/main.yml` or its
        // `handlers/main.yml` — indistinguishable from content alone, so use the handler
        // context: it is a strict superset (Task + `listen`), turning the ambiguity into
        // a missed `listen` diagnostic on task files rather than a false one on handlers.
        Ast::Tasks(build_stmts(items, true))
    }
}

/// Keys of `node` that ansible-core's `_validate_attributes` would reject in `ctx`,
/// after `skip` removes the keys consumed before that check runs.
fn unknown_keys_of(
    node: &Node,
    ctx: KeyContext,
    skip: impl Fn(&str) -> bool,
) -> Vec<UnknownKey> {
    node.entries()
        .iter()
        .filter_map(|(k, _)| {
            let key = k.as_str()?;
            if skip(key) || keywords::legal_key(ctx, key) {
                return None;
            }
            Some(UnknownKey { key: key.to_string(), key_span: k.span(), ctx })
        })
        .collect()
}

fn name_of(node: &Node) -> Option<String> {
    node.get("name").and_then(|n| n.as_str()).map(str::to_owned)
}

/// A `when:` is either one expression or a list of them (ANDed).
fn clauses(when: &Node) -> Vec<String> {
    match when {
        Node::Sequence { items, .. } => items
            .iter()
            .filter_map(|i| i.as_str().map(str::to_owned))
            .collect(),
        other => other.as_str().map(str::to_owned).into_iter().collect(),
    }
}

/// The names written in `node`'s `key` (`notify:` or `listen:`): a scalar is Ansible's
/// one-element-list shorthand, a sequence is one reference per item.
///
/// A non-scalar item is dropped rather than failing the whole list, the opposite of
/// [`loop_items_of`]. The two want different things from a partial read: an iteration that
/// is only partly known is worse than useless, while a name that is only partly known just
/// means one fewer navigable reference — the others are still exactly themselves.
fn handler_refs(node: &Node, key: &str) -> Vec<HandlerRef> {
    let Some(v) = node.get(key) else {
        return Vec::new();
    };
    let one = |n: &Node| {
        n.as_str().map(|s| HandlerRef {
            name: s.to_string(),
            span: n.span(),
            templated: s.contains("{{"),
        })
    };
    match v {
        Node::Sequence { items, .. } => items.iter().filter_map(one).collect(),
        scalar => one(scalar).into_iter().collect(),
    }
}

fn is_looped(node: &Node) -> bool {
    node.entries().iter().any(|(k, _)| {
        matches!(k.as_str(), Some(s) if s == "loop" || s.starts_with("with_"))
    })
}

/// The literal items of a `loop:` list. Empty unless *every* item is a plain scalar with no
/// template in it, so a caller can substitute them and know it has the whole iteration.
///
/// `loop:` only — the `with_*` forms each run a lookup plugin with its own semantics, and
/// guessing those is the kind of unmeasured leap this crate exists to avoid.
fn loop_items_of(node: &Node) -> Vec<String> {
    let Some(Node::Sequence { items, .. }) = node.get("loop") else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(items.len());
    for i in items {
        match i.as_str() {
            Some(s) if !s.contains("{{") => out.push(s.to_string()),
            // One unreadable item makes the whole iteration unreadable: the caller cannot
            // tell a partial list from a complete one, and a partial one is what turns a
            // real value into "not in the set".
            _ => return Vec::new(),
        }
    }
    out
}

/// `vars_files:` entries, plus the items that can never be one. A bare scalar value is
/// Ansible's one-element-list shorthand.
///
/// Only scalars and one level of nesting can name a file: anything deeper, or a non-scalar
/// alternative, fails ansible-core's post-template `isinstance(str)` gate at runtime. Those
/// are returned separately rather than discarded — they are no reference, but they are a
/// diagnostic (T-087), and deciding that twice in two places is how the two come to
/// disagree.
///
/// Two shapes look fatal and are not, both measured on 2.21.2: a null `vars_files:` key
/// and an empty nested list (`- []`) each let the play run, so neither is recorded.
fn vars_files_of(value: &Node) -> (Vec<VarsFilesEntry>, Vec<InvalidVarsFilesEntry>) {
    fn scalar(n: &Node) -> Option<(String, Span)> {
        match n {
            // `Scalar { value: "" }` is an explicitly empty string (`- ''`), not a missing
            // one — a `-` with no value parses as `Node::Null`. It passes the type gate and
            // then resolves to the search dir itself, so it is the directory fault, not
            // this one, and it is left to the resolver.
            Node::Scalar { value, span } if !value.is_empty() => Some((value.clone(), *span)),
            _ => None,
        }
    }
    /// The gate rejects the same two shapes at both levels; only a nested *list* differs,
    /// since one level of nesting is legal and two is not.
    fn invalid(n: &Node, nested_is_fatal: bool) -> Option<InvalidVarsFilesKind> {
        match n {
            Node::Null { .. } => Some(InvalidVarsFilesKind::Null),
            Node::Mapping { .. } => Some(InvalidVarsFilesKind::Mapping),
            // `- []` has nothing in it to fail the gate, and runs.
            Node::Sequence { items, .. } if nested_is_fatal && !items.is_empty() => {
                Some(InvalidVarsFilesKind::Nested)
            }
            _ => None,
        }
    }
    fn entry(n: &Node, bad: &mut Vec<InvalidVarsFilesEntry>) -> Option<VarsFilesEntry> {
        if let Some(kind) = invalid(n, false) {
            bad.push(InvalidVarsFilesEntry { span: n.span(), kind });
            return None;
        }
        match n {
            Node::Scalar { .. } => {
                let (v, s) = scalar(n)?;
                Some(VarsFilesEntry { alternatives: vec![(v, s)], span: s })
            }
            Node::Sequence { items, span } => {
                for item in items {
                    if let Some(kind) = invalid(item, true) {
                        bad.push(InvalidVarsFilesEntry { span: item.span(), kind });
                    }
                }
                let alternatives: Vec<_> = items.iter().filter_map(scalar).collect();
                let end = alternatives.last()?.1.end;
                Some(VarsFilesEntry {
                    alternatives,
                    span: Span { start: span.start, end },
                })
            }
            _ => None,
        }
    }
    let mut bad = Vec::new();
    let good = match value {
        Node::Sequence { items, .. } => {
            items.iter().filter_map(|n| entry(n, &mut bad)).collect()
        }
        Node::Scalar { .. } => entry(value, &mut bad).into_iter().collect(),
        // A null `vars_files:` key runs — measured. Nothing to report.
        _ => Vec::new(),
    };
    (good, bad)
}

/// The `name: value` bindings under a node's `vars:` mapping.
fn vars_of(node: &Node) -> Vec<VarBinding> {
    node.get("vars")
        .map(|m| {
            m.entries()
                .iter()
                .filter_map(|(k, v)| {
                    Some(VarBinding {
                        name: k.as_str()?.to_string(),
                        span: v.span(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Collect the directive keys of `node` that `keep` accepts. Directives are never FQCN,
/// so any dotted key is skipped (it's the module).
fn collect_directives(node: &Node, keep: impl Fn(&str) -> bool) -> Vec<Directive> {
    node.entries()
        .iter()
        .filter_map(|(k, v)| {
            let key = k.as_str()?;
            if key.contains('.') || !keep(key) {
                return None;
            }
            Some(Directive {
                key: key.to_string(),
                key_span: k.span(),
                value: v.span(),
            })
        })
        .collect()
}

fn build_play_item(node: &Node) -> Option<PlayItem> {
    if !matches!(node, Node::Mapping { .. }) {
        return None;
    }
    if let Some(v) = import_playbook_value(node) {
        let when = node.get("when");
        return Some(PlayItem::Import(Import {
            span: v.span(),
            file: v.as_str().map(str::to_owned),
            when: when.map(clauses).unwrap_or_default(),
            when_span: when.map(|w| w.span()),
            vars: import_entry_vars(node),
            directives: collect_directives(node, keywords::is_play_directive),
        }));
    }
    Some(PlayItem::Play(build_play(node)))
}

/// Literal scalars from a `vars:` on an `import_playbook` entry. Non-scalar values are
/// dropped rather than guessed: a nested structure can't be substituted into a path.
fn import_entry_vars(node: &Node) -> Vec<(String, String)> {
    let Some(vars) = node.get("vars") else { return Vec::new() };
    vars.entries()
        .iter()
        .filter_map(|(k, v)| Some((k.as_str()?.to_owned(), v.as_str()?.to_owned())))
        .collect()
}

/// The value of an `import_playbook` key, bare or FQCN. Searched from the end for the same
/// reason as [`Node::get`]: on a duplicate key Ansible imports the last one.
fn import_playbook_value(node: &Node) -> Option<&Node> {
    node.entries()
        .iter()
        .rev()
        .find(|(k, _)| k.as_str().map(keywords::core_action) == Some("import_playbook"))
        .map(|(_, v)| v)
}

fn build_play(node: &Node) -> Play {
    let stmts = |key: &str, handlers: bool| {
        node.get(key)
            .map(|n| build_stmts(n.items(), handlers))
            .unwrap_or_default()
    };
    // Captured structurally, so not repeated in `directives`.
    const STRUCTURAL: &[&str] = &[
        "name",
        "hosts",
        "roles",
        "pre_tasks",
        "tasks",
        "post_tasks",
        "handlers",
    ];
    let vars_files_split =
        node.get("vars_files").map(vars_files_of).unwrap_or_default();
    Play {
        span: node.span(),
        name: name_of(node),
        hosts: node.get("hosts").map(|n| n.span()),
        roles: node.get("roles").map(build_roles).unwrap_or_default(),
        pre_tasks: stmts("pre_tasks", false),
        tasks: stmts("tasks", false),
        post_tasks: stmts("post_tasks", false),
        handlers: stmts("handlers", true),
        vars: vars_of(node),
        vars_files: vars_files_split.0,
        invalid_vars_files: vars_files_split.1,
        directives: collect_directives(node, |k| {
            keywords::is_play_directive(k) && !STRUCTURAL.contains(&k)
        }),
        unknown_keys: unknown_keys_of(node, KeyContext::Play, |_| false),
    }
}

fn build_roles(roles: &Node) -> Vec<RoleUse> {
    roles
        .items()
        .iter()
        .filter_map(|item| match item {
            // - myrole
            Node::Scalar { value, span } => Some(RoleUse {
                name: value.clone(),
                span: *span,
                entry_span: *span,
                params: Vec::new(),
                vars: Vec::new(),
                when: Vec::new(),
                when_span: None,
            }),
            // - role: myrole  /  - name: myrole
            //
            // `name:` is not a label here: `_load_role_name` is
            // `ds.get('role', ds.get('name'))` (`definition.py:118`), so the entry loads
            // the role either way and `role:` only wins when both are written. Reading
            // just `role:` produced no reference at all for the `name:` spelling.
            Node::Mapping { .. } => {
                let named = item
                    .get("role")
                    .or_else(|| item.get("name"))
                    .and_then(|n| match n {
                        Node::Scalar { value, span } if !value.is_empty() => {
                            Some((value.clone(), *span))
                        }
                        // No usable name is fatal upstream ("role definitions must contain
                        // a role name") — no reference to make, and no rule owns it yet.
                        _ => None,
                    });
                let (name, span) = named?;
                Some(RoleUse {
                    name,
                    span,
                    entry_span: item.span(),
                    params: role_params_of(item),
                    vars: vars_of(item),
                    when: item.get("when").map(clauses).unwrap_or_default(),
                    when_span: item.get("when").map(Node::span),
                })
            }
            _ => None,
        })
        .collect()
}

/// The keys of a `roles:` entry that `_split_role_params` would hand to the role as
/// variables — everything `RoleInclude.fattributes` does not claim (`definition.py:207`).
fn role_params_of(item: &Node) -> Vec<RoleParam> {
    item.entries()
        .iter()
        .filter_map(|(k, v)| {
            let key = k.as_str()?;
            if keywords::legal_key(KeyContext::RoleDefinition, key) {
                return None;
            }
            Some(RoleParam {
                name: key.to_string(),
                key_span: k.span(),
                value_span: v.span(),
            })
        })
        .collect()
}

fn build_stmts(items: &[Node], handlers: bool) -> Vec<Stmt> {
    items.iter().filter_map(|n| build_stmt(n, handlers)).collect()
}

fn build_stmt(node: &Node, handlers: bool) -> Option<Stmt> {
    if !matches!(node, Node::Mapping { .. }) {
        return None;
    }
    // `Block.is_block` (`block.py:91-98`): any one of the three makes it a block. Testing only
    // `block:` let a bare `rescue:` fall through to `find_action`, which took the keyword for the
    // module name — so the node read as a well-formed task and every rule downstream stayed
    // silent on it.
    if keywords::BLOCK_TASK_CONTAINERS.iter().any(|k| node.get(k).is_some()) {
        Some(Stmt::Block(build_block(node, handlers)))
    } else {
        Some(Stmt::Task(build_task(node, handlers)))
    }
}

fn build_block(node: &Node, handlers: bool) -> Block {
    let stmts = |key: &str| {
        node.get(key)
            .map(|n| build_stmts(n.items(), handlers))
            .unwrap_or_default()
    };
    let when = node.get("when");
    Block {
        span: node.span(),
        name: name_of(node),
        block: stmts("block"),
        rescue: stmts("rescue"),
        always: stmts("always"),
        when: when.map(clauses).unwrap_or_default(),
        when_span: when.map(|w| w.span()),
        vars: vars_of(node),
        notify: handler_refs(node, "notify"),
        directives: collect_directives(node, |k| {
            keywords::is_block_directive(k)
                && k != "name"
                && k != "notify"
                && !keywords::BLOCK_TASK_CONTAINERS.contains(&k)
        }),
        unknown_keys: unknown_keys_of(node, KeyContext::Block, |_| false),
    }
}

fn build_task(node: &Node, handlers: bool) -> Task {
    let when = node.get("when");
    let action = find_action(node);
    // `include_tasks`/`include_role` tasks are validated against the restricted
    // `VALID_INCLUDE_KEYWORDS` set; `import_*` keep the full Task set
    // (`task_include.py:87-99`, `constants.py:45`).
    let dynamic_include = action
        .as_ref()
        .is_some_and(|a| matches!(keywords::core_action(&a.name), "include_tasks" | "include_role"));
    let ctx = match (handlers, dynamic_include) {
        (true, true) => KeyContext::DynamicHandlerInclude,
        (true, false) => KeyContext::Handler,
        (false, true) => KeyContext::DynamicInclude,
        (false, false) => KeyContext::Task,
    };
    // Keys the args parser consumes before validation ever sees them: any dotted key and
    // the bare module key (`mod_args.py:330-333`), `local_action` (`mod_args.py:131`),
    // and `with_*` (`task.py:336` — accepted by prefix; Ansible additionally requires an
    // installed lookup, which we cannot enumerate, so we err lenient).
    let module_key = action.as_ref().map(|a| a.name.clone());
    let skip = |k: &str| {
        k.contains('.')
            || k.starts_with("with_")
            || k == "local_action"
            || Some(k) == module_key.as_deref()
    };
    let mut unknown_keys = unknown_keys_of(node, ctx, skip);
    if let Some(lc) = node.get("loop_control") {
        unknown_keys.extend(unknown_keys_of(lc, KeyContext::LoopControl, |_| false));
    }
    Task {
        span: node.span(),
        name: name_of(node),
        action,
        when: when.map(clauses).unwrap_or_default(),
        when_span: when.map(|w| w.span()),
        looped: is_looped(node),
        loop_items: loop_items_of(node),
        register: node.get("register").and_then(|n| n.as_str()).map(str::to_owned),
        register_span: node.get("register").map(|n| n.span()),
        vars: vars_of(node),
        notify: handler_refs(node, "notify"),
        listen: handler_refs(node, "listen"),
        directives: collect_directives(node, |k| {
            keywords::is_task_directive(k) && !TASK_STRUCTURAL.contains(&k)
        }),
        unknown_keys,
    }
}

/// Captured structurally on [`Task`], so not repeated in `directives`.
const TASK_STRUCTURAL: &[&str] = &["name", "notify", "listen"];

/// The module on a task: the value of `action:`/`local_action:`, else the first key that
/// isn't a directive (a bare non-directive, or any FQCN).
fn find_action(node: &Node) -> Option<Action> {
    for holder in ["action", "local_action"] {
        if let Some(v) = node.get(holder) {
            if let Some(s) = v.as_str() {
                let name = s.split_whitespace().next().unwrap_or(s).to_string();
                return Some(Action {
                    name,
                    key_span: v.span(),
                    args: v.clone(),
                });
            }
        }
    }
    for (k, v) in node.entries() {
        let Some(key) = k.as_str() else { continue };
        let is_directive = !key.contains('.') && keywords::is_task_directive(key);
        if !is_directive {
            return Some(Action {
                name: key.to_string(),
                key_span: k.span(),
                args: v.clone(),
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::Document;

    fn ast(src: &str) -> Ast {
        build(&Document::new(src.to_string()).parse().expect("valid yaml"))
    }

    fn vars_files(src: &str) -> Vec<VarsFilesEntry> {
        let Ast::Playbook(items) = ast(src) else {
            panic!("expected a playbook");
        };
        let PlayItem::Play(p) = &items[0] else {
            panic!("expected a play");
        };
        p.vars_files.clone()
    }

    fn roles(src: &str) -> Vec<RoleUse> {
        let Ast::Playbook(items) = ast(src) else {
            panic!("expected a playbook");
        };
        let PlayItem::Play(p) = &items[0] else {
            panic!("expected a play");
        };
        p.roles.clone()
    }

    /// T-100. Live-verified on 2.21.2: `- name: definitely_not_a_role` fails with
    /// `The role 'definitely_not_a_role' was not found in: …`, so `name:` is looked up as
    /// a role, not kept as a label. Reading only `role:` produced no reference at all.
    #[test]
    fn name_is_an_alias_for_role_on_a_roles_entry() {
        let got = roles("- hosts: all\n  roles:\n    - name: web\n");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "web");
    }

    /// `ds.get('role', ds.get('name'))` — live-verified: with both written, `role: web`
    /// runs and the bogus `name:` is ignored rather than erroring.
    #[test]
    fn role_wins_over_name_when_both_are_written() {
        let got = roles("- hosts: all\n  roles:\n    - role: web\n      name: just_a_label\n");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "web");
        // `name` is a `Base` attribute, so it is a setting here and never a param.
        assert!(got[0].params.is_empty(), "{:?}", got[0].params);
    }

    /// An empty `role:` shadows a good `name:` rather than falling back to it — upstream's
    /// `ds.get('role', ds.get('name'))` takes the default only when the *key* is absent,
    /// and a written-but-empty `role:` is a present `None`. Live-verified: this is
    /// `role definitions must contain a role name`, not a run of role `web`. The fallback
    /// must stay keyed on absence, not on emptiness.
    #[test]
    fn an_empty_role_key_does_not_fall_back_to_name() {
        assert!(roles("- hosts: all\n  roles:\n    - role:\n      name: web\n").is_empty());
    }

    #[test]
    fn role_params_are_the_keys_outside_the_legal_set() {
        let got = roles(
            "- hosts: all\n  roles:\n    - role: web\n      when: x\n      tasks_from: a.yml\n      \
             port_count: 4\n",
        );
        let names: Vec<_> = got[0].params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["tasks_from", "port_count"]);
    }

    /// A param's two spans anchor two different readers — the key for the diagnostic,
    /// the value for go-to-definition.
    #[test]
    fn a_role_param_spans_its_key_and_its_value() {
        let src = "- hosts: all\n  roles:\n    - role: web\n      tasks_from: alternate.yml\n";
        let got = roles(src);
        let p = &got[0].params[0];
        assert_eq!(p.key_span.slice(src), "tasks_from");
        assert_eq!(p.value_span.slice(src), "alternate.yml");
    }

    /// Upstream raises `role definitions must contain a role name`; there is no reference
    /// to make, so the entry is dropped rather than guessed at.
    #[test]
    fn a_roles_entry_with_no_name_yields_nothing() {
        assert!(roles("- hosts: all\n  roles:\n    - myparam: orphan\n").is_empty());
    }

    #[test]
    fn vars_files_scalar_entries_are_one_alternative_each() {
        let src = "- hosts: all\n  vars_files:\n    - vars/a.yml\n    - b.yml\n";
        let vf = vars_files(src);
        assert_eq!(vf.len(), 2);
        for e in &vf {
            assert_eq!(e.alternatives.len(), 1);
            assert_eq!(e.span, e.alternatives[0].1);
        }
        assert_eq!(vf[0].alternatives[0].0, "vars/a.yml");
        assert_eq!(vf[0].span.slice(src), "vars/a.yml");
        assert_eq!(vf[1].span.slice(src), "b.yml");
    }

    #[test]
    fn vars_files_bare_string_and_null_forms() {
        let vf = vars_files("- hosts: all\n  vars_files: vars/a.yml\n");
        assert_eq!(vf.len(), 1);
        assert_eq!(vf[0].alternatives[0].0, "vars/a.yml");
        assert!(vars_files("- hosts: all\n  vars_files:\n  tasks: []\n").is_empty());
    }

    #[test]
    fn vars_files_nested_list_keeps_alternatives_and_group_span() {
        let src = "- hosts: all\n  vars_files:\n    - - a.yml\n      - b.yml\n    - c.yml\n";
        let vf = vars_files(src);
        assert_eq!(vf.len(), 2);
        assert_eq!(vf[0].alternatives.len(), 2);
        assert_eq!(vf[0].alternatives[1].0, "b.yml");
        // The group span covers the whole nested list from its first `-`, clamped to the
        // last alternative — libyaml's block-sequence end mark would otherwise spill onto
        // the next line.
        assert_eq!(vf[0].span.slice(src), "- a.yml\n      - b.yml");
        assert_eq!(vf[1].alternatives.len(), 1);
    }

    #[test]
    fn vars_files_flow_style_groups() {
        let src = "- hosts: all\n  vars_files: [[a.yml, b.yml], c.yml]\n";
        let vf = vars_files(src);
        assert_eq!(vf.len(), 2);
        assert_eq!(vf[0].alternatives.len(), 2);
        assert_eq!(vf[1].alternatives[0].0, "c.yml");
    }

    #[test]
    fn vars_files_non_scalar_and_deeper_nesting_are_dropped() {
        // A mapping entry, a doubly-nested list, and an empty inner list are all
        // runtime-fatal (or empty) in Ansible — none can name a file, so no entry.
        let src = r#"
            - hosts: all
              vars_files:
                - {a: b}
                - - - deep.yml
                - []
                - ok.yml
"#;
        let vf = vars_files(src);
        assert_eq!(vf.len(), 1);
        assert_eq!(vf[0].alternatives[0].0, "ok.yml");
    }

    #[test]
    fn playbook_with_plays_and_import() {
        let a = ast(
            "- name: web\n  hosts: web\n  roles: [nginx, {role: certs}]\n  tasks:\n    - debug: {msg: hi}\n\
             - import_playbook: other.yml\n",
        );
        let Ast::Playbook(items) = a else {
            panic!("expected a playbook, got {a:?}");
        };
        assert_eq!(items.len(), 2);
        let PlayItem::Play(play) = &items[0] else {
            panic!("first item should be a play");
        };
        assert_eq!(play.name.as_deref(), Some("web"));
        assert!(play.hosts.is_some());
        assert_eq!(play.roles.len(), 2);
        assert_eq!(play.roles[0].name, "nginx");
        assert_eq!(play.roles[1].name, "certs");
        assert_eq!(play.tasks.len(), 1);
        let Stmt::Task(t) = &play.tasks[0] else {
            panic!("expected a task");
        };
        assert_eq!(t.action.as_ref().unwrap().name, "debug");

        let PlayItem::Import(imp) = &items[1] else {
            panic!("second item should be an import");
        };
        assert_eq!(imp.file.as_deref(), Some("other.yml"));
    }

    /// A duplicate key is legal YAML and Ansible keeps the last. Live-verified on
    /// ansible-core 2.21.2: with both `first.yml` and `last.yml` on one entry it warns,
    /// then runs only the tasks from `last.yml`.
    #[test]
    fn duplicate_import_playbook_takes_the_last() {
        let a = ast("- import_playbook: first.yml\n  import_playbook: last.yml\n");
        let Ast::Playbook(items) = a else { panic!("expected a playbook") };
        let PlayItem::Import(imp) = &items[0] else { panic!("expected an import") };
        assert_eq!(imp.file.as_deref(), Some("last.yml"));
    }

    /// Same rule one level down: the `vars:` on the entry is a mapping too, so a repeated
    /// name there resolves to the last value.
    #[test]
    fn duplicate_entry_var_takes_the_last() {
        let a = ast("- import_playbook: \"{{ env }}.yml\"\n  vars:\n    env: dead\n    env: live\n");
        let Ast::Playbook(items) = a else { panic!("expected a playbook") };
        let PlayItem::Import(imp) = &items[0] else { panic!("expected an import") };
        // Both pairs are carried; the resolver folds them into a map, which is last-wins.
        let folded: std::collections::HashMap<_, _> = imp.vars.iter().cloned().collect();
        assert_eq!(folded.get("env").map(String::as_str), Some("live"));
    }

    #[test]
    fn task_file_is_a_flat_list() {
        let a = ast("- name: one\n  command: echo hi\n- ansible.builtin.debug:\n    msg: two\n");
        let Ast::Tasks(stmts) = a else {
            panic!("expected a task list, got {a:?}");
        };
        assert_eq!(stmts.len(), 2);
        let Stmt::Task(t0) = &stmts[0] else { panic!() };
        assert_eq!(t0.name.as_deref(), Some("one"));
        assert_eq!(t0.action.as_ref().unwrap().name, "command");
        let Stmt::Task(t1) = &stmts[1] else { panic!() };
        assert_eq!(t1.action.as_ref().unwrap().name, "ansible.builtin.debug");
    }

    #[test]
    fn module_is_separated_from_directives() {
        let a = ast(r#"
            - name: t
              when: x is defined
              loop: [1, 2]
              register: out
              command: echo hi
"#);
        let Ast::Tasks(stmts) = a else { panic!() };
        let Stmt::Task(t) = &stmts[0] else { panic!() };
        assert_eq!(t.action.as_ref().unwrap().name, "command");
        // when/loop/register are directives; name is captured separately.
        let keys: Vec<&str> = t.directives.iter().map(|d| d.key.as_str()).collect();
        assert!(keys.contains(&"when"));
        assert!(keys.contains(&"loop"));
        assert!(keys.contains(&"register"));
        assert!(!keys.contains(&"command"), "command is the module, not a directive");
        assert!(!keys.contains(&"name"));
    }

    #[test]
    fn block_with_rescue_and_always() {
        let a = ast(r#"
            - block:
                - debug: {msg: try}
              rescue:
                - debug: {msg: catch}
              always:
                - debug: {msg: fin}
              when: risky
"#);
        let Ast::Tasks(stmts) = a else { panic!() };
        let Stmt::Block(b) = &stmts[0] else {
            panic!("expected a block");
        };
        assert_eq!(b.block.len(), 1);
        assert_eq!(b.rescue.len(), 1);
        assert_eq!(b.always.len(), 1);
        assert!(b.directives.iter().any(|d| d.key == "when"));
    }

    /// `Block.is_block` is any of the three, so a `rescue:` with no `block:` is a malformed
    /// block — not a task calling a module named `rescue`. Testing `block:` alone let the
    /// keyword through to `find_action`, which named it as the action and left `unknown_keys`
    /// empty, so the node read as well-formed and every downstream rule stayed silent.
    #[test]
    fn rescue_or_always_alone_is_still_a_block() {
        for key in ["rescue", "always"] {
            let a = ast(&format!("- {key}:\n    - debug: {{msg: x}}\n"));
            let Ast::Tasks(stmts) = a else { panic!() };
            let Stmt::Block(b) = &stmts[0] else {
                panic!("{key} alone should classify as a block");
            };
            assert!(b.block.is_empty());
            assert_eq!(b.rescue.len() + b.always.len(), 1);
        }
    }

    /// The same shape nested one level deeper. Upstream loses the classification here — the
    /// inner walker tests `'block' in task_ds` (`helpers.py:104`) rather than `is_block`, and
    /// reports an internal class name at the user. We give row 7's message in both positions.
    #[test]
    fn a_nested_rescue_only_mapping_is_a_block_too() {
        let a = ast("- block:\n    - rescue:\n        - debug: {msg: x}\n");
        let Ast::Tasks(stmts) = a else { panic!() };
        let Stmt::Block(outer) = &stmts[0] else { panic!("expected a block") };
        let Stmt::Block(inner) = &outer.block[0] else {
            panic!("expected the nested rescue to be a block")
        };
        assert!(inner.block.is_empty());
        assert_eq!(inner.rescue.len(), 1);
    }

    #[test]
    fn action_form_names_the_module() {
        let a = ast("- name: legacy\n  action: command echo hi\n");
        let Ast::Tasks(stmts) = a else { panic!() };
        let Stmt::Task(t) = &stmts[0] else { panic!() };
        assert_eq!(t.action.as_ref().unwrap().name, "command");
    }

    #[test]
    fn vars_file_is_other() {
        assert!(matches!(ast("key: value\nother: 2\n"), Ast::Other));
    }

    fn first_play(a: &Ast) -> &Play {
        let Ast::Playbook(items) = a else { panic!("expected a playbook") };
        let PlayItem::Play(p) = &items[0] else { panic!("expected a play") };
        p
    }

    #[test]
    fn play_only_keys_are_rejected_on_a_play() {
        let a = ast("- hosts: web\n  when: x is defined\n  user: alice\n  tasks:\n    - debug:\n");
        let p = first_play(&a);
        let keys: Vec<&str> = p.unknown_keys.iter().map(|u| u.key.as_str()).collect();
        // `when:` on a play is fatal (Play mixes in no Conditional, `play.py:48`)...
        assert_eq!(keys, ["when"]);
        assert_eq!(p.unknown_keys[0].ctx, KeyContext::Play);
        // ...while legacy `user:` is renamed in preprocess and accepted (`play.py:166-174`).
    }

    #[test]
    fn loop_on_a_block_is_rejected() {
        let a = ast("- hosts: web\n  tasks:\n    - block:\n        - debug:\n      loop: [1, 2]\n");
        let Stmt::Block(b) = &first_play(&a).tasks[0] else { panic!("expected a block") };
        let keys: Vec<&str> = b.unknown_keys.iter().map(|u| u.key.as_str()).collect();
        assert_eq!(keys, ["loop"]);
        assert_eq!(b.unknown_keys[0].ctx, KeyContext::Block);
    }

    #[test]
    fn the_module_key_with_loops_and_local_action_are_never_unknown() {
        let a = ast(
            "- hosts: web\n  tasks:\n    - community.general.ufw: {rule: allow}\n      \
             with_items: [a]\n    - local_action: command echo hi\n      register: out\n",
        );
        let p = first_play(&a);
        for s in &p.tasks {
            let Stmt::Task(t) = s else { panic!() };
            assert!(t.unknown_keys.is_empty(), "found: {:?}", t.unknown_keys);
        }
    }

    #[test]
    fn a_misplaced_task_key_is_unknown_with_the_task_context() {
        let a = ast("- hosts: web\n  tasks:\n    - debug:\n      listen: restart nginx\n");
        let Stmt::Task(t) = &first_play(&a).tasks[0] else { panic!() };
        let keys: Vec<&str> = t.unknown_keys.iter().map(|u| u.key.as_str()).collect();
        // `listen` is handler-only (`handler.py:27`).
        assert_eq!(keys, ["listen"]);
        assert_eq!(t.unknown_keys[0].ctx, KeyContext::Task);
    }

    #[test]
    fn handlers_accept_listen() {
        let a = ast("- hosts: web\n  handlers:\n    - name: restart\n      debug:\n      listen: x\n");
        let Stmt::Task(t) = &first_play(&a).handlers[0] else { panic!() };
        assert!(t.unknown_keys.is_empty(), "found: {:?}", t.unknown_keys);
    }

    #[test]
    fn dynamic_includes_use_the_restricted_set() {
        // `become:` is a perfectly good task key, but invalid on `include_tasks`
        // (`task_include.py:42-44,87-99`); the import form keeps the full set.
        let a = ast(
            "- hosts: web\n  tasks:\n    - include_tasks: f.yml\n      become: true\n    \
             - import_tasks: f.yml\n      become: true\n",
        );
        let p = first_play(&a);
        let Stmt::Task(inc) = &p.tasks[0] else { panic!() };
        let keys: Vec<&str> = inc.unknown_keys.iter().map(|u| u.key.as_str()).collect();
        assert_eq!(keys, ["become"]);
        assert_eq!(inc.unknown_keys[0].ctx, KeyContext::DynamicInclude);
        let Stmt::Task(imp) = &p.tasks[1] else { panic!() };
        assert!(imp.unknown_keys.is_empty(), "found: {:?}", imp.unknown_keys);
    }

    #[test]
    fn loop_control_keys_are_their_own_context() {
        let a = ast(
            "- hosts: web\n  tasks:\n    - debug:\n      loop: [1]\n      loop_control:\n        \
             loop_var: it\n        pause: 1\n        name: nope\n",
        );
        let Stmt::Task(t) = &first_play(&a).tasks[0] else { panic!() };
        let keys: Vec<&str> = t.unknown_keys.iter().map(|u| u.key.as_str()).collect();
        // `name` is Base vocabulary; LoopControl skips Base entirely (`loop_control.py:27`).
        assert_eq!(keys, ["name"]);
        assert_eq!(t.unknown_keys[0].ctx, KeyContext::LoopControl);
    }

    /// The list form is several independent references, so each name gets its own span —
    /// a single span over the sequence could not underline the one that missed.
    #[test]
    fn notify_carries_one_ref_per_written_name() {
        let src = "- hosts: web\n  tasks:\n    - command: echo hi\n      notify:\n        \
                   - restart nginx\n        - reload haproxy\n";
        let a = ast(src);
        let Stmt::Task(t) = &first_play(&a).tasks[0] else { panic!() };
        let names: Vec<&str> = t.notify.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["restart nginx", "reload haproxy"]);
        // The span must cover its own name and nothing else.
        for n in &t.notify {
            assert_eq!(&src[n.span.start..n.span.end], n.name);
        }
    }

    /// The scalar spelling is Ansible's one-element-list shorthand, so it must produce the
    /// same shape as the list — a consumer reading `notify` should never branch on which
    /// spelling was written.
    #[test]
    fn a_scalar_notify_is_a_one_element_list() {
        let src = "- hosts: web\n  tasks:\n    - command: echo hi\n      notify: restart nginx\n";
        let a = ast(src);
        let Stmt::Task(t) = &first_play(&a).tasks[0] else { panic!() };
        assert_eq!(t.notify.len(), 1);
        assert_eq!(t.notify[0].name, "restart nginx");
        assert_eq!(&src[t.notify[0].span.start..t.notify[0].span.end], "restart nginx");
        assert!(!t.notify[0].templated);
    }

    /// Per name, not per key: one templated entry in a list must not mark its literal
    /// siblings, or a rule that skips templated names would go silent on the whole task.
    #[test]
    fn templated_is_per_name_not_per_key() {
        let a = ast(
            "- hosts: web\n  tasks:\n    - command: echo hi\n      notify:\n        \
             - restart {{ svc }}\n        - reload haproxy\n",
        );
        let Stmt::Task(t) = &first_play(&a).tasks[0] else { panic!() };
        let flags: Vec<bool> = t.notify.iter().map(|n| n.templated).collect();
        assert_eq!(flags, [true, false]);
    }

    /// `listen:` is the other half of the index — a topic is a valid `notify:` target, so
    /// it has to be readable by name the same way.
    #[test]
    fn listen_is_captured_on_a_handler() {
        let src = "- hosts: web\n  handlers:\n    - name: h\n      debug:\n      listen:\n        \
                   - restart web\n        - restart all\n";
        let a = ast(src);
        let Stmt::Task(t) = &first_play(&a).handlers[0] else { panic!() };
        let names: Vec<&str> = t.listen.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["restart web", "restart all"]);
        assert!(t.notify.is_empty());
    }

    /// Both keys are captured structurally, so they must leave `directives` — the
    /// convention the play's `STRUCTURAL` list already follows. Two representations of one
    /// key is how a consumer ends up reading the stale one.
    #[test]
    fn notify_and_listen_leave_the_directive_list() {
        let a = ast(
            "- hosts: web\n  handlers:\n    - name: h\n      debug:\n      listen: topic\n      \
             notify: other\n      when: x\n",
        );
        let Stmt::Task(t) = &first_play(&a).handlers[0] else { panic!() };
        let keys: Vec<&str> = t.directives.iter().map(|d| d.key.as_str()).collect();
        assert!(!keys.contains(&"notify"), "found: {keys:?}");
        assert!(!keys.contains(&"listen"), "found: {keys:?}");
        // The control: an unrelated directive is still collected, so the filter is not
        // simply emptying the list.
        assert!(keys.contains(&"when"), "found: {keys:?}");
    }

    /// `notify:` is legal on a block and inherits to every task inside it; `listen:` is
    /// not in Block's set at all. The asymmetry is Ansible's, so the model keeps it rather
    /// than smoothing it over with a field that could never be filled.
    #[test]
    fn a_block_carries_notify_and_rejects_listen() {
        let a = ast(
            "- hosts: web\n  tasks:\n    - block:\n        - debug: {msg: x}\n      \
             notify: restart nginx\n      listen: nope\n",
        );
        let Stmt::Block(b) = &first_play(&a).tasks[0] else { panic!("expected a block") };
        assert_eq!(b.notify.len(), 1);
        assert_eq!(b.notify[0].name, "restart nginx");
        let keys: Vec<&str> = b.directives.iter().map(|d| d.key.as_str()).collect();
        assert!(!keys.contains(&"notify"), "found: {keys:?}");
        let unknown: Vec<&str> = b.unknown_keys.iter().map(|u| u.key.as_str()).collect();
        assert_eq!(unknown, ["listen"]);
    }

    /// A non-scalar entry drops itself and leaves its siblings intact — the opposite of
    /// `loop:`, where one unreadable item voids the list. Each name here is independently
    /// exactly itself, so there is nothing for a partial read to falsify.
    #[test]
    fn an_unreadable_notify_entry_drops_only_itself() {
        let a = ast(
            "- hosts: web\n  tasks:\n    - command: echo hi\n      notify:\n        - good\n        \
             - {a: b}\n        - also good\n",
        );
        let Stmt::Task(t) = &first_play(&a).tasks[0] else { panic!() };
        let names: Vec<&str> = t.notify.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["good", "also good"]);
    }

    #[test]
    fn a_standalone_task_file_gets_the_handler_superset() {
        // tasks/main.yml and handlers/main.yml are indistinguishable by content, so
        // `listen` passes here — a deliberate miss instead of a false error on handlers.
        let a = ast("- debug:\n  listen: x\n- include_tasks: f.yml\n  listen: y\n");
        let Ast::Tasks(stmts) = a else { panic!() };
        for s in &stmts {
            let Stmt::Task(t) = s else { panic!() };
            assert!(t.unknown_keys.is_empty(), "found: {:?}", t.unknown_keys);
        }
    }
}

