//! Where variables are *defined* — the substrate for variable go-to-definition and the
//! "is this ever set?" checks.
//!
//! [`index`] covers one parsed file: play/block/task `vars:`, `set_fact:`, `register:`.
//! [`definitions`] extends that across files by deterministic paths only — role
//! `defaults/`/`vars/`, `vars_files:`, `include_vars:` (file and dir forms, via the ported
//! module semantics in [`crate::include_vars`]), playbook-adjacent
//! `group_vars/`/`host_vars/`, and the `set_fact`/`register` in included task files and
//! roles, and — since T-062 — the inventory itself, including the `group_vars/`/`host_vars/`
//! kept beside it. Still not covered: variables injected by a caller, and the opaque runtime
//! sources (a dynamic inventory we refuse to execute, inventory host-matching, `-e`). So a
//! name absent here is *not* proof it's undefined.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::ast::{self, Ast, Block, Play, PlayItem, Stmt, Task};
use crate::cache::{Contribution, ScanCache};
use crate::fs::Fs;
use crate::condition;
use crate::include_vars;
use crate::parse::{Node, Span};
use crate::references::{self, ReferenceKind};
use crate::resolve;
use crate::workspace::FileContext;

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
    /// A variable written in the inventory itself — a `[group:vars]` section, an inline
    /// `var=value` on a host line, or `vars:`/`hosts:` in a YAML inventory (T-062).
    /// Host-dependent like [`VarSource::GroupVars`]: which hosts it reaches depends on the
    /// group it sits in, which needs the inventory's own group membership to answer.
    Inventory,
    /// A key loaded by an `include_vars:` task (file or dir form).
    IncludeVars,
    /// A param on a play's `roles:` entry — any key the entry wrote that
    /// `RoleInclude.fattributes` does not claim (T-100).
    RoleParams,
    /// A key under a `vars:` written on a play's `roles:` entry. Distinct from
    /// [`VarSource::RoleParams`] because it is the documented spelling and because it is
    /// combined *after* the params (`role/__init__.py:552-558`), so it wins a collision.
    RoleEntryVars,
    /// An argument key on an `add_host:` task — a host variable on the hosts that task
    /// creates (T-177). Host-scoped like [`VarSource::Inventory`], and for the same reason:
    /// the hosts are named at runtime (`name: "{{ item }}"` over a loop), so which hosts it
    /// reaches is not answerable from the file.
    AddHost,
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
            // Ansible publishes inventory group vars at 6 and inventory host vars at 10.
            // We do not know which a given entry is without resolving group membership,
            // so the lower of the two is used: it can only lose a precedence tie, never
            // wrongly win one.
            VarSource::Inventory => 6,
            VarSource::HostVars => 10,
            // Level 8, "inventory file or script host vars" — bracketed on both sides by
            // measurement rather than read off the published table, since a host var set at
            // runtime is not obviously the same rung as one read from an inventory file. One
            // run, every source defining the same name on the created host, each also
            // contributing a unique name so a collision the loser never entered cannot read
            // as a win: add_host beat playbook `group_vars/` (7) and lost to `host_vars/`
            // (9), play `vars:` (12) and `set_fact` (19).
            VarSource::AddHost => 8,
            VarSource::PlayVars => 12,
            VarSource::VarsFiles => 14,
            VarSource::RoleVars => 15,
            VarSource::BlockVars => 16,
            VarSource::TaskVars => 17,
            VarSource::IncludeVars => 18,
            VarSource::SetFact | VarSource::Register => 19,
            VarSource::RoleParams => 20,
            // Level 15 — the same bucket as `vars/main.yml`, which it wins by being
            // combined after it inside one `get_vars()` (`role/__init__.py:549-558`).
            // Measured, and the reading order in that function is misleading: `self.vars`
            // is combined *last* there, but role params still beat it, because params are
            // a separate published level (20) applied outside the bucket. So entry vars
            // beat `vars/main.yml` and lose to a param of the same name — verified both
            // ways round, in both write orders.
            VarSource::RoleEntryVars => 15,
        }
    }

    /// True for sources whose applicability depends on the target host — a named `group_vars`
    /// or `host_vars` file. We can point at the definition, but not assert it's in effect for
    /// a given host without parsing inventory.
    ///
    /// [`VarSource::AddHost`] joins them for a stronger reason than inventory's: its hosts
    /// are named at runtime, so no amount of parsing enumerates them.
    pub fn host_scoped(self) -> bool {
        matches!(
            self,
            VarSource::GroupVars
                | VarSource::HostVars
                | VarSource::Inventory
                | VarSource::AddHost
        )
    }

    /// Whether a `hostvars[...]` read can see this source (T-104).
    ///
    /// `HostVars.raw_get` calls `get_vars(host=host, include_hostvars=False)` with **no
    /// play and no task** (`vars/hostvars.py:53`), so the split is not a precedence level:
    /// it is whether the source wrote into the *host's* storage or hung off the
    /// play/role/task object. All thirteen measured on 2.21.2, cross-host:
    ///
    /// | visible                                   | invisible                            |
    /// | ----------------------------------------- | ------------------------------------ |
    /// | `group_vars/`, `host_vars/`               | play `vars:`, `vars_files:`          |
    /// | `include_vars`                            | role defaults, role vars             |
    /// | `set_fact`, `register`                    | role params, `roles:` entry `vars:`  |
    /// |                                           | block `vars:`, task `vars:`          |
    ///
    /// `include_vars` visible while `vars_files` is not is the one to remember: both load a
    /// YAML file of variables, and only the first calls `register_host_variables`
    /// (`action/include_vars.py:149`, the same door `set_fact` uses). Reading the levels
    /// instead of measuring would put `include_vars` on the wrong side and warn on working
    /// code.
    ///
    /// `add_host` measured separately (T-177) and visible, with both controls alive in the
    /// one run: `hostvars['newhost'].addhost_var` read back its value, while a play var on
    /// the same run read `UNDEF` through the same expression — so the probe could report
    /// either way. Precedence does not decide this: it writes into the host's storage.
    pub fn visible_to_hostvars(self) -> bool {
        match self {
            VarSource::GroupVarsAll
            | VarSource::GroupVars
            | VarSource::HostVars
            | VarSource::Inventory
            | VarSource::IncludeVars
            | VarSource::SetFact
            | VarSource::AddHost
            | VarSource::Register => true,
            VarSource::PlayVars
            | VarSource::BlockVars
            | VarSource::TaskVars
            | VarSource::VarsFiles
            | VarSource::RoleDefaults
            | VarSource::RoleVars
            | VarSource::RoleParams
            | VarSource::RoleEntryVars => false,
        }
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
    /// Byte range in the defining file outside which this definition does not apply.
    /// `None` for everything file-wide, which is nearly all of them. Set for a `roles:`
    /// entry's params and `vars:`, which reach the rest of that entry and the role's own
    /// files but *not* the play's tasks (measured) — so a use after the roles must not be
    /// satisfied by one, in the diagnostic or in a hover.
    pub scope: Option<Span>,
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
        self.defs.push(VarDef { name: name.into(), source, span, condition, scope: None });
    }

    fn push_scoped(&mut self, name: impl Into<String>, source: VarSource, span: Span, scope: Span) {
        self.defs.push(VarDef {
            name: name.into(),
            source,
            span,
            condition: None,
            scope: Some(scope),
        });
    }
}

/// A place a variable is *used*, with the absolute span of the name.
#[derive(Debug, Clone)]
pub struct VarUse {
    pub name: String,
    pub span: Span,
    /// The `when:` clauses guarding the task this use sits in (accumulated through nesting),
    /// so a conditional use can be checked against its definitions' conditions.
    pub guard: Vec<String>,
    /// Set by [`undefined_uses`] when a definition of this name *does* exist in the file
    /// but does not reach here — a `roles:` entry's params and `vars:` read from the play's
    /// tasks (T-100), or a play-scoped source read through `hostvars` (T-104). The
    /// distinction is the whole message: "never defined" sends the reader to add a
    /// definition that is already eleven lines up.
    pub defined_out_of_scope: bool,
    /// Read through `hostvars[...]`, which is assembled with no play and no task — so only
    /// the sources [`VarSource::visible_to_hostvars`] admits can satisfy it (T-104).
    pub through_hostvars: bool,
}

/// Variable uses inside `{{ }}` templates in `text`. `base` is the byte offset of `text`
/// in the document, so the returned spans are absolute. Literal text outside `{{ }}` is
/// not scanned — only a template expression references variables.
pub fn template_uses(text: &str, base: usize, out: &mut Vec<VarUse>) {
    template_uses_with(text, base, out, condition::variable_uses)
}

/// Which words count is the caller's choice — see [`Extract`].
fn template_uses_with(
    text: &str,
    base: usize,
    out: &mut Vec<VarUse>,
    extract: impl Fn(&str) -> Vec<(String, usize, usize)>,
) {
    let mut i = 0;
    while let Some(open) = text[i..].find("{{") {
        let expr_start = i + open + 2;
        let Some(close_rel) = text[expr_start..].find("}}") else {
            break;
        };
        let expr = &text[expr_start..expr_start + close_rel];
        push_uses(expr, base + expr_start, out, &extract);
        i = expr_start + close_rel + 2;
    }
}

/// Variable uses in a bare Jinja expression (a `when:` clause), where the whole string is
/// the expression rather than literal text with `{{ }}` islands.
pub fn expression_uses(expr: &str, base: usize, out: &mut Vec<VarUse>) {
    expression_uses_with(expr, base, out, condition::variable_uses)
}

fn expression_uses_with(
    expr: &str,
    base: usize,
    out: &mut Vec<VarUse>,
    extract: impl Fn(&str) -> Vec<(String, usize, usize)>,
) {
    push_uses(expr, base, out, &extract);
}

/// One Jinja expression's uses: the roots `extract` finds, plus the names read off a
/// `hostvars[...]` lookup, which the root scan structurally cannot see (T-104). Both views
/// (`uses` and `any_uses`) come through here, so neither can acquire the hostvars names
/// without the other.
fn push_uses(
    expr: &str,
    base: usize,
    out: &mut Vec<VarUse>,
    extract: &impl Fn(&str) -> Vec<(String, usize, usize)>,
) {
    let mut push = |name: String, s: usize, e: usize, through_hostvars: bool| {
        out.push(VarUse {
            name,
            span: Span { start: base + s, end: base + e },
            guard: Vec::new(),
            defined_out_of_scope: false,
            through_hostvars,
        });
    };
    for (name, s, e) in extract(expr) {
        push(name, s, e, false);
    }
    // Guarded on the substring: this runs per scalar of every file in a scan, and the
    // overwhelming majority never mention the name.
    if expr.contains("hostvars") {
        for (name, s, e) in condition::hostvars_uses(expr) {
            push(name, s, e, true);
        }
    }
}

/// Every variable use in a parsed file. A `when:` value is treated as one expression; any
/// other scalar is treated as literal text with `{{ }}` templates. Walks the raw tree so
/// every scalar's span is exact, and accumulates the enclosing `when:` onto each use.
pub fn uses(nodes: &[Node]) -> Vec<VarUse> {
    uses_with(nodes, condition::variable_uses)
}

/// [`uses`] plus the names ansible provides, which it drops on purpose — no rule can use a
/// name no workspace file defines. Hover and go-to-definition take this view: a definition
/// answers first, and only a name with none falls back to the injected table
/// ([`crate::injected`]), so a user's own `ansible_custom` is never hidden by its prefix
/// (T-143, T-224).
pub fn any_uses(nodes: &[Node]) -> Vec<VarUse> {
    uses_with(nodes, condition::any_uses)
}

/// Generic, not a `fn` pointer: this runs once per scalar of every file in a scan, and the
/// indirection would cost the tokenizer its inlining.
fn uses_with(
    nodes: &[Node],
    extract: impl Fn(&str) -> Vec<(String, usize, usize)> + Copy,
) -> Vec<VarUse> {
    let mut out = Vec::new();
    for n in nodes {
        walk_uses(n, false, &[], &mut out, extract, Keys::Literal);
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

/// Which of a mapping's keys Ansible renders as templates (T-169). Exactly two action
/// plugins do — `set_fact`'s own args and `set_stats`'s `data:` — and upstream calls it
/// "a rare case where key templating is allowed" (`action/set_fact.py:44`). Everywhere
/// else the braces in a key are not a variable use, measured on 2.21.2: an unknown module
/// parameter is fatal (`Unsupported parameters … {{ argname }}`, so nothing was rendered)
/// and a key nested in ordinary data keeps its braces verbatim. Scanning keys wholesale
/// would invent uses Ansible never resolves.
#[derive(Clone, Copy, PartialEq)]
enum Keys {
    /// Literal — every mapping but the two below.
    Literal,
    /// This mapping's own scalar keys are rendered.
    Templated,
    /// The `data:` child's keys are rendered (`set_stats`).
    UnderData,
}

/// What the value under key `k` inherits. The two openers fire only from [`Keys::Literal`]:
/// the values inside a templated mapping are fact data, never tasks, so a fact *named*
/// `set_fact` cannot open a second templated level.
fn keys_under(current: Keys, k: &Node) -> Keys {
    match (current, k.as_str().map(crate::keywords::core_action)) {
        (Keys::Literal, Some("set_fact")) => Keys::Templated,
        (Keys::Literal, Some("set_stats")) => Keys::UnderData,
        (Keys::UnderData, Some("data")) => Keys::Templated,
        _ => Keys::Literal,
    }
}

fn walk_uses(
    node: &Node,
    in_when: bool,
    guard: &[String],
    out: &mut Vec<VarUse>,
    ex: impl Fn(&str) -> Vec<(String, usize, usize)> + Copy,
    keys: Keys,
) {
    match node {
        Node::Scalar { value, span } => {
            let before = out.len();
            if in_when {
                expression_uses_with(value, span.start, out, ex);
            } else {
                template_uses_with(value, span.start, out, ex);
            }
            for u in &mut out[before..] {
                u.guard = guard.to_vec();
            }
        }
        // Neither key-templating site is list-shaped, so items start over as literal.
        Node::Sequence { items, .. } => {
            items.iter().for_each(|i| walk_uses(i, in_when, guard, out, ex, Keys::Literal))
        }
        Node::Mapping { entries, .. } => {
            // This task/block's own `when:` guards the values inside it (its module args),
            // accumulated onto whatever guard we inherited.
            let mut inner = guard.to_vec();
            inner.extend(when_of(node));
            for (k, v) in entries {
                let is_when = k.as_str().map(crate::keywords::core_action) == Some("when");
                // The `when:` expression itself isn't guarded by itself — use the outer guard.
                let g: &[String] = if is_when { guard } else { &inner };
                if keys == Keys::Templated {
                    if let Node::Scalar { value, span } = k {
                        let before = out.len();
                        template_uses_with(value, span.start, out, ex);
                        for u in &mut out[before..] {
                            u.guard = inner.clone();
                        }
                    }
                }
                walk_uses(v, is_when, g, out, ex, keys_under(keys, k));
            }
        }
        // Null holds no text, so there is nothing to scan for variable uses.
        Node::Null { .. } | Node::Other { .. } => {}
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
    // Role params. Their real scope is the role, not the play — measured: a param is not
    // visible to the play's own `tasks:` after the role runs. Indexing them play-wide is
    // therefore an over-approximation, in the direction this index is allowed to err:
    // it can only make `undefined_uses` quieter, never produce a false "undefined". The
    // reverse direction — a role file seeing the params its callers pass — is the useful
    // one and needs the invocation chain (T-020).
    // Params first, then the entry's `vars:` — the order ansible combines them in, so a
    // name written both ways lands with the winner last.
    for r in &p.roles {
        for param in &r.params {
            idx.push_scoped(
                param.name.clone(),
                VarSource::RoleParams,
                param.value_span,
                r.entry_span,
            );
        }
        for v in &r.vars {
            idx.push_scoped(v.name.clone(), VarSource::RoleEntryVars, v.span, r.entry_span);
        }
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

/// The `add_host:` argument keys consumed as module parameters, so not host variables.
///
/// Upstream's own `special_args` (`action/add_host.py:85`), and **not** the module's alias
/// list, which is where reading the docs goes wrong. `host:` and `group:` are accepted as
/// aliases when the plugin picks the host name (`:52`) and the group list (`:68`), and are
/// then left in `args` — so each also lands in `host_vars` as an ordinary variable.
///
/// Measured on 2.21.2 by diffing each spelling's created host against a `name:`-only
/// baseline, which is what makes it evidence rather than a reading: `host:` left `host`
/// behind and `group:` left `group`, while `hostname:`, `groups:` and `groupname:` left
/// nothing — and all five created their host and group, so the aliases do work.
const ADD_HOST_PARAMS: [&str; 4] = ["name", "hostname", "groupname", "groups"];

fn task(t: &Task, idx: &mut VarIndex) {
    // A task's `when:` guards everything it defines — so the variable is only set on the
    // hosts/runs where the condition holds.
    let cond = (!t.when.is_empty()).then(|| t.when.join(" and "));
    for v in &t.vars {
        idx.push_cond(v.name.clone(), VarSource::TaskVars, v.span, cond.clone());
    }
    if let Some(a) = &t.action {
        if crate::keywords::core_action(&a.name) == "set_fact" {
            for (fact, _) in a.args.entries() {
                if let Some(name) = fact.as_str() {
                    // `cacheable` is a set_fact option, not a fact. A templated key names
                    // the fact only once rendered (T-169) — filing the literal would put a
                    // definition in the index under a name no expression can reference,
                    // since braces are not variable-name characters. Which name it really
                    // creates needs the template evaluated, which is T-034's; a miss beats
                    // a definition nothing can reach.
                    if name != "cacheable" && !name.contains("{{") {
                        idx.push_cond(name, VarSource::SetFact, fact.span(), cond.clone());
                    }
                }
            }
        }
        // The only other module whose argument keys are variable definitions (T-177).
        //
        // Mapping args only. The free-form spelling `add_host: name=h ff_var=V` really does
        // define `ff_var` — measured, it read back in the next play — and `entries()` is
        // empty for a scalar, so this misses it and a use elsewhere still reports undefined.
        // Splitting that string is `parse_kv`/shlex semantics, which is T-046's, and a
        // hand-rolled version is the kind of unmeasured guess this file exists to avoid.
        // Pinned by `the_free_form_add_host_spelling_is_a_known_miss`.
        if crate::keywords::core_action(&a.name) == "add_host" {
            for (key, value) in a.args.entries() {
                if let Some(name) = key.as_str() {
                    // Templated keys define nothing reachable, for the reason set_fact's do
                    // not: measured, the host variable is really named `{{ k }}` (T-170).
                    if !ADD_HOST_PARAMS.contains(&name) && !name.contains("{{") {
                        // The *value* span, with play vars and against `set_fact`, which is
                        // the closer-looking shape. Both are one line apart so the jump is
                        // the same either way, and only this side lets hover read the value
                        // out — which is worth having here and not for `set_fact`, whose
                        // values are templates far more often than literals.
                        idx.push_cond(name, VarSource::AddHost, value.span(), cond.clone());
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
    /// The chain of edges that pulled the defining file into scope, when that route isn't
    /// written in the file being read — today only `meta/main.yml` dependencies (T-066).
    /// Each element is (meta file, span of the dependency entry), ordered from the read
    /// file outward — the order you'd follow the links. Empty when the route is visible
    /// (in-file, direct role, include). Mirrors Ansible's own `dep_chain`.
    pub via: Vec<(PathBuf, Span)>,
    /// See [`VarDef::scope`].
    pub scope: Option<Span>,
    /// The point in the run this definition starts applying, as (file, byte offset) of the
    /// task that loads it — set for `include_vars` only (T-208).
    ///
    /// `set_fact`/`register` need no such field: their `file`/`span` already *are* the task,
    /// so [`ordered_before`](Located::ordered_before) compares those directly. An
    /// `include_vars` definition's `file`/`span` point into the **loaded vars file** instead,
    /// which is a different file from the use and carries no information about when the
    /// include ran — so without this the ordering test could not fire at all.
    pub after: Option<(PathBuf, usize)>,
}

impl Located {
    /// Whether this definition can be in effect at a use at byte `use_pos` in `use_file`.
    ///
    /// Play/block/task `vars:`, `vars_files:` and role defaults/vars bind before the tasks
    /// run, so they always apply. `set_fact`/`register` happen at a point in the run: in the
    /// *same* file, one after the use hasn't executed yet, so it can't define that use. Across
    /// files we can't order it against the use, so we keep it rather than guess.
    pub fn in_effect_at(&self, use_file: &Path, use_pos: usize) -> bool {
        self.in_scope_at(use_file, use_pos) && self.ordered_before(use_file, use_pos)
    }

    /// The run-order half of [`in_effect_at`], split out so the use-aware pair below can
    /// reuse it without re-deriving the scope test.
    fn ordered_before(&self, use_file: &Path, use_pos: usize) -> bool {
        // Loaded partway through the run by a task we recorded the position of. Same
        // across-files rule as below: if the loading task is in another file we cannot order
        // the two, so the definition is kept rather than guessed away.
        if let Some((at_file, at_pos)) = &self.after {
            return at_file != use_file || *at_pos < use_pos;
        }
        match self.source {
            VarSource::SetFact | VarSource::Register => {
                self.file != use_file || self.span.start < use_pos
            }
            _ => true,
        }
    }

    /// Can this definition satisfy `use_` at all? Scope (T-100), plus — for a read through
    /// `hostvars[...]` — whether the source survives into host storage (T-104).
    ///
    /// Both rules live here rather than in one caller, which is the T-100 lesson: the scope
    /// check once sat in `undefined_uses` alone, and hover went on pointing at a definition
    /// the warning called missing. A second reachability rule split the same way would
    /// reproduce that exactly.
    pub fn reaches(&self, use_: &VarUse, use_file: &Path) -> bool {
        self.in_scope_at(use_file, use_.span.start)
            && (!use_.through_hostvars || self.source.visible_to_hostvars())
    }

    /// [`reaches`] plus run order — what hover and go-to-definition want, since they answer
    /// "what does this read *here*" rather than "is this name ever set".
    pub fn in_effect_for(&self, use_: &VarUse, use_file: &Path) -> bool {
        self.reaches(use_, use_file) && self.ordered_before(use_file, use_.span.start)
    }

    /// Whether this definition's [`scope`](Located::scope) covers a use at `use_pos` in
    /// `use_file`. Separate from [`in_effect_at`](Located::in_effect_at) because the two
    /// callers want different halves: `undefined_uses` lets *any* reachable definition
    /// exempt a use, even a `set_fact` written later — ordering is the uncovered-`when`
    /// check's business — but a definition that does not reach the use *at all* must still
    /// not exempt it. A use in another file is the role-invocation direction, which
    /// nothing reaches today and T-020 will answer; not something to rule out here.
    pub fn in_scope_at(&self, use_file: &Path, use_pos: usize) -> bool {
        match self.scope {
            Some(s) if self.file == use_file => s.start <= use_pos && use_pos < s.end,
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
            // Load order before position. `span.start` is an offset *within* a file, so it
            // orders two definitions in the same one and means nothing across two — it let
            // whichever file happened to carry the variable further down win. Ansible merges
            // a vars directory's files with `combine_vars` in sorted order, so the later path
            // wins: measured, `a.yml` padded past `b.yml`'s offset still resolves to `b.yml`.
            //
            // This only settles same-precedence collisions. Two group_vars *directories* —
            // one beside the playbook, one beside the inventory — are different Ansible
            // levels (5 vs 4), not a tie, and we label both `GroupVars`; see T-175.
            .then(a.file.cmp(&b.file))
            .then(a.span.start.cmp(&b.span.start))
    })
}

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
    known_literals_in(defs, path, text, &ScanCache::default())
}

/// [`known_literals`] reading the defining files through a scan cache, so the files the
/// walk already read aren't read again per consumer (T-076).
pub fn known_literals_in(
    defs: &[Located],
    path: &Path,
    text: &str,
    cache: &ScanCache,
) -> std::collections::HashMap<String, Vec<String>> {
    use std::collections::HashMap;
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for d in defs {
        if !value_span_source(d.source) {
            continue;
        }
        let value = if d.file == path {
            d.span.slice(text).trim().to_string()
        } else {
            match cache.source(&d.file) {
                Some(src) => d.span.slice(&src.text).trim().to_string(),
                None => String::new(),
            }
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
    undefined_uses_in(path, nodes, text, &ScanCache::default())
}

/// [`undefined_uses`] sharing a scan cache across the files of one pass (T-076).
pub fn undefined_uses_in(
    path: &Path,
    nodes: &[Node],
    text: &str,
    cache: &ScanCache,
) -> Vec<VarUse> {
    let tree = ast::build(nodes);
    if !matches!(tree, Ast::Playbook(_)) {
        return Vec::new();
    }
    // Scope is the definition's own business now (`Located::scope`), so a use is checked
    // against the definitions actually in effect *at that byte*, not against a flat set of
    // every name the file mentions. That is what keeps `app_config_dir: {{ app_env }}`
    // inside an entry silent while the same name in the play's tasks is reported.
    let all = definitions_with_deps_in(path, nodes, cache).0;
    let declared = declared_names(nodes, text);
    uses(nodes)
        .into_iter()
        .filter(|u| {
            !all.iter().any(|d| d.name == u.name && d.reaches(u, path))
                // A `hostvars[...]` read is answered mostly by inventory, which we do not
                // parse (T-062) — so this check has nothing to say about one, in either
                // direction. Measured, both ways round:
                //
                // - found nothing: 37 corpus warnings, all 37 inventory host vars.
                // - found only play-scoped definitions: still not provably undefined. A
                //   name set in play `vars:` *and* in inventory reads fine through
                //   hostvars — measured, it returns the inventory value — so "always
                //   undefined" is a false claim on working code.
                //
                // Hover and go-to-definition still apply `visible_to_hostvars`, which is
                // sound for them: the play var is definitely not what this read returns,
                // whatever inventory holds. Claiming the read is *broken* needs T-062.
                && !u.through_hostvars
                && !crate::injected::provided(&u.name)
                && !declared.contains(&u.name)
                && !u.guard.iter().any(|g| g.contains(&u.name) && g.contains("defined"))
                && !softened(text, u.span.start)
        })
        .map(|mut u| {
            u.defined_out_of_scope = all.iter().any(|d| d.name == u.name);
            u
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
    definitions_with_deps_in(path, nodes, &ScanCache::default())
}

/// [`definitions_with_deps`] sharing a [`ScanCache`] with the other files of the same pass,
/// so a subtree several files reach is read, parsed and walked once for all of them (T-076).
/// A fresh cache gives exactly [`definitions_with_deps`].
pub fn definitions_with_deps_in(
    path: &Path,
    nodes: &[Node],
    cache: &ScanCache,
) -> (Vec<Located>, HashSet<PathBuf>) {
    let c = contribution_in(path, nodes, cache);
    (c.defs, c.deps)
}

/// The hosts an `add_host` anywhere in `path`'s reachable set creates, or `None` when that
/// set is not enumerable (T-179).
///
/// Reachability is the whole question, and both halves are measured on 2.21.2: a role's
/// `add_host` **is** visible to the playbook that uses the role, and an `add_host` in a file
/// nothing includes is **not** — reading it back is fatal with the same bare
/// `hostvars['orphanhost']` an unknown host gives. So this rides the definitions walk, which
/// already follows exactly those edges and memoizes the result, rather than scanning the
/// workspace or re-traversing the graph.
///
/// `None` is the same third answer [`inventory_hosts`] returns, and for the same reason: a
/// templated name or an unresolvable include edge means hosts exist that cannot be named, and
/// a caller answering from the partial set would report a typo on a real host.
pub fn created_hosts_in(
    path: &Path,
    nodes: &[Node],
    cache: &ScanCache,
) -> Option<HashSet<String>> {
    let c = contribution_in(path, nodes, cache);
    (!c.hosts_unknowable).then_some(c.created_hosts)
}

fn contribution_in(path: &Path, nodes: &[Node], cache: &ScanCache) -> Contribution {
    let mut walk = Walk {
        cache,
        // The root is on the stack from the start: a subtree that loops back to it is
        // truncated here exactly as before, and — because that makes its result incomplete
        // for anyone else — the taint that rule sets is what keeps it out of the cache.
        stack: HashSet::new(),
        truncated: false,
    };
    if let Some(hit) = cache.contribution(path) {
        cache.count_defs(hit.defs.len());
        return (*hit).clone();
    }
    let mut c = Contribution::default();
    if let Some(canon) = cache.canonical(path) {
        walk.stack.insert(canon.clone());
        c.deps.insert(canon);
    }
    collect(path, nodes, &mut c, &mut walk);
    dedup(&mut c.defs);
    cache.count_defs(c.defs.len());
    // The root's walk is a contribution like any other — memoize it so the *next* file whose
    // subtree reaches this one gets it for free. Only when nothing truncated it, and only
    // when the caller's `nodes` are what's on disk; a scan's are (it skips open buffers) and
    // an editing path uses a throwaway cache, so this can't publish an unsaved parse.
    if !walk.truncated {
        cache.store(
            path.to_path_buf(),
            Arc::new(Contribution {
                defs: c.defs.clone(),
                deps: c.deps.clone(),
                created_hosts: c.created_hosts.clone(),
                hosts_unknowable: c.hosts_unknowable,
            }),
        );
    }
    c
}

/// Same var reached by two routes (a vars file two plays share, a role listed twice)
/// collapses. **First occurrence wins**, and that is load-bearing: the earlier route is the
/// one Ansible actually executes, so its provenance is the one to keep (see the
/// meta-dependency block in [`collect`]). Keys borrow rather than clone — this runs over
/// every definition every file can see, which is the one part memoization can't remove.
/// Record which task loaded the definitions added since `start`, so [`Located::ordered_before`]
/// can tell a use above the include from one below it (T-208).
///
/// Everything in the range is freshly read and unstamped — [`read_var_file`] walks a vars
/// file's key/value mappings and never recurses into tasks, so an `include_vars` cannot nest
/// inside one. Checked with an assertion over the whole suite and the demo tree before this
/// was written as an unconditional overwrite.
fn stamp_include_site(out: &mut Contribution, start: usize, site: &(PathBuf, usize)) {
    for d in &mut out.defs[start..] {
        d.after = Some(site.clone());
    }
}

/// Drop a definition already collected — a shared subtree reached twice contributes the same
/// names twice, once per route.
///
/// `source` is part of the key (T-207). One file can be loaded at two precedence levels — a
/// role's `vars/main.yml` re-read by `include_vars:` is 15 *and* 18, and a `vars_files:` entry
/// re-read the same way is 14 *and* 18 — and both are real: the higher one is what Ansible
/// resolves against, the lower one is why the file was in scope at all. Keyed without
/// `source`, the second was dropped and the index kept whichever route ran first, which is the
/// level **not** in effect.
///
/// Two routes to the same load still collapse, because they carry the same `source` — that is
/// what this function is for, and the first-route-wins rule that goes with it (see the `via`
/// comment in [`collect`]) is unchanged.
fn dedup(defs: &mut Vec<Located>) {
    let keep: Vec<bool> = {
        let mut seen: HashSet<(&str, &Path, usize, VarSource)> = HashSet::new();
        defs.iter()
            .map(|d| seen.insert((d.name.as_str(), d.file.as_path(), d.span.start, d.source)))
            .collect()
    };
    let mut it = keep.into_iter();
    defs.retain(|_| it.next().unwrap_or(true));
}

/// One walk's own state. The cache is shared with the rest of the scan; the stack is not —
/// "am I inside a cycle" is a property of this walk, and treating another thread's
/// in-flight file as a cycle would wrongly truncate.
struct Walk<'a> {
    cache: &'a ScanCache,
    /// Files whose contribution is being computed right now, innermost frames included.
    stack: HashSet<PathBuf>,
    /// Set when a frame stopped at a file already on the stack. Its result is missing
    /// whatever that file would have added, so it is right for *this* walk and wrong for
    /// any other — it must not be memoized, and neither must any frame above it.
    truncated: bool,
}

fn collect(path: &Path, nodes: &[Node], out: &mut Contribution, walk: &mut Walk) {
    let ctx = walk.cache.context(path);
    let tree = ast::build(nodes);

    // In-file definitions (play/block/task vars, set_fact, register).
    for d in index(&tree).defs() {
        out.defs.push(Located {
            name: d.name.clone(),
            source: d.source,
            span: d.span,
            file: path.to_path_buf(),
            after: None,
            condition: d.condition.clone(),
            via: Vec::new(),
            scope: d.scope,
        });
    }

    // The enclosing role's defaults/ and vars/ — fixed locations, no search.
    if let Some(role) = &ctx.role_dir {
        read_var_file(&role.join("defaults").join("main.yml"), VarSource::RoleDefaults, None, out, walk);
        read_var_file(&role.join("vars").join("main.yml"), VarSource::RoleVars, None, out, walk);

        // meta/main.yml dependencies run before this role, so their defaults/vars and
        // set_facts are in scope here — and, transitively, for whoever calls this role
        // (entering a dependency's files rediscovers *its* role context and deps).
        {
            let meta = role.join("meta").join("main.yml");
            if let Some(mnodes) = walk.cache.source(&meta).and_then(|s| s.nodes.clone()) {
                {
                    let mctx = walk.cache.context(&meta);
                    for dep in references::meta_dependencies(&mnodes) {
                        let start = out.defs.len();
                        let resolver = resolve::Resolver { fs: walk.cache, ..Default::default() };
                        for target in resolver.resolve(&dep, &mctx).targets {
                            let files = role_task_files(&target, walk);
                            for f in files.iter() {
                                collect_disk(f, out, walk);
                            }
                        }
                        // Provenance chain (T-066): everything this dependency's walk added
                        // arrived through an edge invisible from the hovered file. Deeper
                        // recursion has already stamped its own edges, so prepending here
                        // builds each def's chain outermost-first — the order a reader
                        // follows the links from where they're hovering.
                        //
                        // When a role is reachable both here and directly, both routes now
                        // contribute (the memo has no per-walk `visited` to swallow the
                        // second) and the final dedup keeps whichever came first —
                        // deliberately: Ansible compiles both copies and skips the second at
                        // runtime per host (play_iterator.py "role has already run",
                        // allow_duplicates false by default for roles:/deps), so the first
                        // route IS the one that executes, and its breadcrumb is the one to
                        // keep.
                        for d in &mut out.defs[start..] {
                            d.via.insert(0, (meta.clone(), dep.span));
                        }
                    }
                }
            }
        }
    }

    // Play-level vars_files — explicit paths written in the play. A nested entry is
    // first-found: Ansible loads only the winning alternative, so only it is indexed.
    // A templated alternative abandons the whole entry — were it to resolve at runtime
    // it would win, so indexing a later literal could credit the wrong file.
    if let Ast::Playbook(items) = &tree {
        for it in items {
            if let PlayItem::Play(p) = it {
                for entry in &p.vars_files {
                    for (alt, _) in &entry.alternatives {
                        if alt.contains("{{") {
                            break;
                        }
                        let hit = crate::resolve::vars_files_candidates(alt, &ctx.file_dir)
                            .into_iter()
                            .find(|p| walk.cache.is_file(p));
                        if let Some(f) = hit {
                            read_var_file(&f, VarSource::VarsFiles, None, out, walk);
                            break;
                        }
                    }
                }
            }
        }
    }

    // The hosts `add_host` creates in this file (T-179). Collected here rather than in
    // `task()`, which owns the *variable* index and has no view of the contribution — and
    // this has to travel across include edges, which is what the contribution is for.
    each_task(&tree, &mut |t| {
        let Some(a) = &t.action else { return };
        if crate::keywords::core_action(&a.name) != "add_host" {
            return;
        }
        // All three spellings of the host name. `host` and `hostname` are the module's own
        // aliases (`aliases: [host, hostname]`) and all three were measured to create their
        // host — reading only two of them made `add_host: {host: x}` unknowable, which
        // silenced the whole file for a name sitting in plain view.
        const NAME_KEYS: [&str; 3] = ["name", "host", "hostname"];
        let named = NAME_KEYS.iter().find_map(|k| a.args.get(k)).and_then(|n| n.as_str());
        let named = match named {
            Some(n) => Some(n.to_string()),
            // The free-form spelling, `add_host: name=h ansible_connection=local`, which
            // really does create the host — measured. `entries()` is empty for a scalar, so
            // this used to fall through to unknowable; `parse_kv` is the shlex-based splitter
            // ansible itself uses, already ported for `include_vars`, so there is nothing to
            // hand-roll here.
            None => match &a.args {
                Node::Scalar { value, .. } => crate::splitter::parse_kv(value, false)
                    .ok()
                    .and_then(|kv| {
                        NAME_KEYS.iter().find_map(|k| kv.get(k).map(str::to_owned))
                    }),
                _ => None,
            },
        };
        let Some(name) = named else {
            out.hosts_unknowable = true;
            return;
        };
        match Some(name.as_str()) {
            // A templated name over a *literal* loop is readable: substitute each item and
            // the whole iteration is enumerable. Measured — `name: "{{ item }}"` with
            // `loop: ['loopa','loopb']` creates both, and `"{{ item }}-web"` creates
            // `a-web`/`b-web`, so the substitution is textual and covers a suffix for free.
            //
            // Anything still holding a `{{` after that is not resolved — a second variable
            // in the name, or `loop_control: loop_var:` renaming `item` out from under this
            // — and falls through to unknowable. That guard is what lets the unhandled
            // iteration forms (`with_*`, a templated `loop:`) stay correct without being
            // enumerated here: they yield no items, nothing substitutes, the `{{` survives.
            Some(n) if n.contains("{{") && !t.loop_items.is_empty() => {
                let expanded: Vec<String> = t
                    .loop_items
                    .iter()
                    .map(|i| n.replace("{{ item }}", i).replace("{{item}}", i))
                    .collect();
                if expanded.iter().any(|e| e.contains("{{")) {
                    out.hosts_unknowable = true;
                } else {
                    out.created_hosts.extend(expanded);
                }
            }
            // `name: "{{ item }}"` over something unreadable — the case T-177 came from,
            // where the loop is a `k8s_info` result. The host is real and its name is not.
            Some(n) if n.contains("{{") => out.hosts_unknowable = true,
            // Taken whole. A comma looks like a host list and is not one — measured,
            // `name: "alpha,beta"` creates a single host *named* `alpha,beta`, so splitting
            // would register two hosts that do not exist and silence a real typo on either.
            Some(n) => {
                out.created_hosts.insert(n.to_string());
            }
            None => out.hosts_unknowable = true,
        }
    });

    // include_vars tasks — load a file or a directory at a point in the play. The task's
    // when: guards the load, so it carries a condition. Templated targets are skipped.
    each_task(&tree, &mut |t| {
        let Some(a) = &t.action else { return };
        if crate::keywords::core_action(&a.name) != "include_vars" {
            return;
        }
        let cond = (!t.when.is_empty()).then(|| t.when.join(" and "));
        let Some(params) = include_vars::params_from_args(&a.args) else { return };
        // Where the run reaches this load (T-208). Stamped on whatever the read below adds,
        // the way `via` is stamped on a dependency's contribution — the loaded file's own
        // spans say nothing about when the include ran.
        let start = out.defs.len();
        let site = (path.to_path_buf(), t.span.start);
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
                    include_vars::load(&params, &ictx, walk.cache)
                {
                    for f in &l.files {
                        read_var_file(f, VarSource::IncludeVars, cond.clone(), out, walk);
                    }
                }
            }
            stamp_include_site(out, start, &site);
            return;
        }
        let file = match &a.args {
            Node::Scalar { value, .. } => Some(value.as_str()),
            Node::Mapping { .. } => a.args.get("file").and_then(|n| n.as_str()),
            _ => None,
        };
        if let Some(f) = file {
            if !f.contains("{{") {
                if let Some(path) = resolve_var_path(f, &ctx, walk.cache) {
                    read_var_file(&path, VarSource::IncludeVars, cond, out, walk);
                }
            }
        }
        stamp_include_site(out, start, &site);
    });

    // Playbook-adjacent group_vars/ and host_vars/ — a fixed location next to this file, not
    // a workspace scan. The inventory-adjacent copies (next to a separate inventory file)
    // need the inventory's location, which we don't guess, so those stay unindexed.
    read_var_dir(&ctx.file_dir.join("group_vars"), true, out, walk);
    read_var_dir(&ctx.file_dir.join("host_vars"), false, out, walk);

    // The inventory itself (T-062) — the single largest source of names this index used to
    // be blind to, and the reason `var-undefined` concedes inventory in every message.
    // `sources` resolves the same ladder Ansible does; a dynamic one is skipped, never run.
    for src in crate::inventory::sources(&ctx.config, walk.cache) {
        read_inventory(&src, out, walk);
    }
    // The `group_vars/`/`host_vars/` beside the *inventory*, which are a different pair from
    // the playbook-adjacent ones read above — Ansible loads both. Keyed on the configured
    // source's base directory, not on each expanded file's parent: a directory source keeps
    // its base at the top however deep the host files sit inside it.
    for dir in crate::inventory::source_dirs(&ctx.config, walk.cache) {
        if dir != ctx.file_dir {
            read_var_dir(&dir.join("group_vars"), true, out, walk);
            read_var_dir(&dir.join("host_vars"), false, out, walk);
        }
    }

    // Follow includes and roles so set_fact/register/vars in those files count too. The
    // enclosing-role rule above then also picks up each reached role's defaults/vars.
    {
        let extracted = references::extract(nodes);
        let resolver = resolve::Resolver {
            fs: walk.cache,
            in_playbook: extracted.in_playbook,
            ..Default::default()
        };
        for r in extracted.refs {
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
            let resolved = resolver.resolve(&r, &ctx);
            // A templated edge is a hole in the reachable set: whatever `{{ kind }}.yml`
            // turns out to be may call `add_host`, so the created hosts stop being
            // enumerable here (T-179). Not permanent — a value set derived from a
            // dominating `assert` would close it (T-180) — but unreadable today.
            //
            // Only this flag is set, never `truncated`: the *definitions* half is unchanged
            // by a skipped edge and stays cacheable exactly as before.
            if resolved.skip_reason == Some(resolve::SkipReason::Templated) {
                out.hosts_unknowable = true;
            }
            for target in resolved.targets {
                if r.kind == ReferenceKind::Role {
                    let files = role_task_files(&target, walk);
                    for f in files.iter() {
                        collect_disk(f, out, walk);
                    }
                } else {
                    collect_disk(&target, out, walk);
                }
            }
        }
    }
}

/// Follow one edge into `path`: serve its contribution from the scan cache when it's there,
/// otherwise walk it once and put it there. What's merged is always a *clone*, because the
/// caller may stamp provenance on it (T-066) and the cached copy must stay raw.
fn collect_disk(path: &Path, out: &mut Contribution, walk: &mut Walk) {
    let Some(canon) = walk.cache.canonical(path) else { return };
    // Keyed by the path as written, not by `canon`: a file's contribution is derived from
    // the spelling it was reached by — `file:` on each def, and the directory its
    // `group_vars/` is looked up in — so two spellings of one file are two contributions.
    // `canon` is for identity questions only: is this a cycle, and which files were read.
    if let Some(hit) = walk.cache.contribution(path) {
        merge(out, &hit);
        return;
    }
    if !walk.stack.insert(canon.clone()) {
        // Already being walked further up this stack — a cycle. Its defs land in that
        // frame; stopping here is what makes the walk terminate. Flag the truncation so
        // nothing computed under it gets memoized.
        walk.truncated = true;
        return;
    }
    let mut sub = Contribution::default();
    sub.deps.insert(canon.clone());
    if let Some(nodes) = walk.cache.source(path).and_then(|s| s.nodes.clone()) {
        let outer = std::mem::replace(&mut walk.truncated, false);
        collect(path, &nodes, &mut sub, walk);
        let truncated = walk.truncated;
        walk.truncated = outer || truncated;
        walk.stack.remove(&canon);
        // Collapse duplicates here rather than only at the root: a shared subtree reached
        // twice from inside this file is dropped once, for every consumer of the memo.
        dedup(&mut sub.defs);
        let sub = Arc::new(sub);
        if truncated {
            walk.cache.count_uncached();
        } else {
            walk.cache.store(path.to_path_buf(), sub.clone());
        }
        merge(out, &sub);
    } else {
        // Unreadable or unparseable: still a dependency (a fix to it must invalidate), and
        // still worth remembering so the next consumer doesn't retry the parse.
        walk.stack.remove(&canon);
        let sub = Arc::new(sub);
        walk.cache.store(path.to_path_buf(), sub.clone());
        merge(out, &sub);
    }
}

fn merge(out: &mut Contribution, from: &Contribution) {
    out.defs.extend(from.defs.iter().cloned());
    out.deps.extend(from.deps.iter().cloned());
    out.created_hosts.extend(from.created_hosts.iter().cloned());
    // One unknowable subtree makes the whole set unknowable: the files that *did* resolve
    // cannot vouch for the hosts the one that didn't would have contributed.
    out.hosts_unknowable |= from.hosts_unknowable;
}

/// A role contributes every task file it has (`tasks_from` reaches beyond `main.yml`).
fn role_task_files(role_main: &Path, walk: &Walk) -> Arc<Vec<PathBuf>> {
    match role_main.parent() {
        Some(tasks_dir) => walk.cache.tree(tasks_dir),
        None => Arc::new(vec![role_main.to_path_buf()]),
    }
}

/// Read every `*.yml` in a playbook-adjacent `group_vars/` or `host_vars/` directory. The
/// source is derived from the file name: `group_vars/all` applies to all hosts, any other
/// name is group- or host-scoped.
fn read_var_dir(dir: &Path, group: bool, out: &mut Contribution, walk: &mut Walk) {
    let files = walk.cache.listing(dir);
    for p in files.iter() {
        // The entity is the `group_vars/<entity>` entry, not the leaf file: everything under
        // `group_vars/all/` belongs to `all`, however deep, so reading the leaf's stem
        // classified `group_vars/all/a.yml` as an ordinary group and lost `all`'s lower
        // precedence rank.
        let stem = p
            .strip_prefix(dir)
            .ok()
            .and_then(|rel| rel.components().next())
            .and_then(|c| Path::new(c.as_os_str()).file_stem())
            .and_then(|s| s.to_str())
            .unwrap_or("");
        let source = if !group {
            VarSource::HostVars
        } else if stem == "all" {
            VarSource::GroupVarsAll
        } else {
            VarSource::GroupVars
        };
        read_var_file(p, source, None, out, walk);
    }
}

/// The inventory sources resolved for `path` that were **declined** — a plugin config or a
/// script, which we detect and never run.
///
/// Skipping one is not the same as reading one that turned out to be empty, and the whole
/// point of recording it is that those two states are otherwise identical downstream. An
/// empty host list means "there is no such host"; a declined source means "the host list is
/// unknowable". Anything that answers a host-existence question has to tell them apart or it
/// will confidently report a false error in exactly the workspaces — cloud inventories —
/// where it has the least standing to.
///
/// Recomputed rather than carried on [`Contribution`]: the sources come from `path`'s own
/// config, so this is a property of one file's context, not something a subtree contributes
/// upward. Everything it touches is already memoized in the cache, so asking is cheap.
pub fn declined_inventories(path: &Path, cache: &ScanCache) -> Vec<PathBuf> {
    let ctx = cache.context(path);
    crate::inventory::sources(&ctx.config, cache)
        .into_iter()
        .filter(|src| {
            cache.source(src).is_some_and(|s| {
                let nodes: &[Node] = s.nodes.as_deref().map_or(&[], |n| n.as_slice());
                crate::inventory::classify(src, &s.text, nodes, cache)
                    == crate::inventory::Kind::Dynamic
            })
        })
        .collect()
}

/// Every host the resolved inventories declare, or `None` when the list is **unknowable**.
///
/// The `None` is the whole design. A caller asking "is there a host called `web0143`?" gets
/// three answers, not two — yes, no, and *I cannot tell* — and only the middle one may become
/// a diagnostic. Collapsing the third into "no" is how a tool comes to report a confident
/// false error in precisely the workspaces it understands least.
///
/// Unknowable means: no inventory resolved at all (the run's `-i` is invisible to an editor),
/// a source we could not read, or any **dynamic** source — a plugin config or a script, which
/// we detect and refuse to execute, so its hosts exist only in an account we are not calling.
/// That last case is what [`declined_inventories`] was built to keep separable from an
/// inventory that was read and simply has no such host.
pub fn inventory_hosts(path: &Path, cache: &ScanCache) -> Option<HashSet<String>> {
    let ctx = cache.context(path);
    let sources = crate::inventory::sources(&ctx.config, cache);
    if sources.is_empty() {
        return None;
    }
    let mut out = HashSet::new();
    for src in sources {
        let s = cache.source(&src)?;
        let nodes: &[Node] = s.nodes.as_deref().map_or(&[], |n| n.as_slice());
        match crate::inventory::classify(&src, &s.text, nodes, cache) {
            crate::inventory::Kind::Dynamic => return None,
            // `?` on each: a reader gives up when one pattern expands past
            // `MAX_PATTERN_HOSTS`, and that has to reach the caller as "unknowable" rather
            // than as a short list, which would report every host past the cap as a typo.
            crate::inventory::Kind::Toml => out.extend(crate::inventory::toml_hosts(&s.text)?),
            crate::inventory::Kind::Yaml => out.extend(crate::inventory::yaml_hosts(nodes)?),
            crate::inventory::Kind::Ini => out.extend(crate::inventory::ini_hosts(&s.text)?),
        }
    }
    // An empty list is not a finding. It means every source parsed to nothing — a file
    // ansible discards outright (box 6's unknown section tag) reads exactly like an inventory
    // with no hosts, and neither is standing to call a name a typo.
    (!out.is_empty()).then_some(out)
}

/// Is this file loaded by the `host_group_vars` vars plugin rather than parsed as an
/// inventory source — i.e. does it sit under a `group_vars/`/`host_vars/` directory?
///
/// Any depth, because the plugin reads a whole entity directory recursively — the same
/// reason [`read_var_dir`] takes the entity from the first component and not the leaf.
fn under_vars_plugin_dir(file: &Path) -> bool {
    file.ancestors()
        .skip(1)
        .any(|a| matches!(a.file_name().and_then(|n| n.to_str()), Some("group_vars" | "host_vars")))
}

/// Every top-level `ansible_group_priority` in a `group_vars/`/`host_vars/` file, by the span
/// of the **key** — the thing that does nothing. Empty for any other file.
///
/// Measured on 2.21.2, two same-depth groups defining one name, with a control that came out
/// the other way. The winner is the alphabetically later group unless priority moves it:
///
/// | `ansible_group_priority: 10` set in | merge winner       | visible as a variable |
/// | ----------------------------------- | ------------------ | --------------------- |
/// | nowhere (baseline)                  | `zulu`             | absent                |
/// | ini inventory `[alpha:vars]`        | **`alpha`** — honoured | absent            |
/// | yaml inventory `alpha:`'s `vars:`   | **`alpha`** — honoured | absent            |
/// | `group_vars/alpha.yml`              | `zulu` — **ignored**   | `10`              |
/// | `host_vars/node1.yml`               | `zulu` — **ignored**   | `10`              |
/// | ini inventory host line             | `zulu` — **ignored**   | `10`              |
///
/// The last column is why this needs saying at all: where the key works it is *consumed*
/// (`Group.set_variable`, `inventory/group.py:216-217`) and never becomes a variable, and
/// where it is inert it survives as an ordinary one — so the only visible evidence points
/// the wrong way. Vars-plugin output is merged after inventory parsing and bypasses
/// `set_variable` entirely (`inventory/manager.py:248-249`).
///
/// Priority only orders groups at the **same depth**. Measured on 2.21.2 with
/// `[parent:children]` holding `alpha` and a same-depth sibling `zulu`: the baseline winner is
/// `alpha` (the deeper group), and giving the parent `ansible_group_priority=20` against the
/// child's `1` still leaves `alpha` the winner. A child always overrides its parent, so
/// nesting never changes whether this key is a variable — which is why one gate per reader
/// covers every depth.
///
/// Top-level only: in a vars file every top-level key is a variable, and a nested one is
/// just data that was never a candidate for the merge-order slot.
pub fn ignored_group_priority(file: &Path, nodes: &[Node]) -> Vec<Span> {
    if !under_vars_plugin_dir(file) {
        return Vec::new();
    }
    nodes
        .iter()
        .flat_map(|n| n.entries())
        .filter(|(k, _)| k.as_str() == Some(crate::inventory::GROUP_PRIORITY))
        .map(|(k, _)| k.span())
        .collect()
}

/// Index one inventory source. INI and YAML shapes both go through
/// [`crate::inventory`]; a dynamic one contributes nothing, because learning its hosts
/// would mean executing a file out of the workspace. The skip is observable through
/// [`declined_inventories`], which is what keeps "no hosts" and "hosts unknown" apart.
fn read_inventory(file: &Path, out: &mut Contribution, walk: &mut Walk) {
    let Some(src) = walk.cache.source(file) else {
        return;
    };
    if let Some(c) = &src.canon {
        out.deps.insert(c.clone());
    }
    let nodes: &[Node] = src.nodes.as_deref().map_or(&[], |n| n.as_slice());
    let vars = match crate::inventory::classify(file, &src.text, nodes, walk.cache) {
        // A dynamic inventory is the one source we decline: running a plugin against a
        // live account to learn a host list is not a trade worth making.
        crate::inventory::Kind::Dynamic => return,
        crate::inventory::Kind::Toml => crate::inventory::toml_vars(&src.text),
        crate::inventory::Kind::Yaml => crate::inventory::yaml_vars(nodes),
        crate::inventory::Kind::Ini => crate::inventory::ini_vars(&src.text),
    };
    for v in vars {
        out.defs.push(Located {
            name: v.name,
            source: VarSource::Inventory,
            span: v.span,
            file: file.to_path_buf(),
            after: None,
            condition: None,
            via: Vec::new(),
            // An inventory applies wherever it is loaded; which *hosts* it reaches is the
            // host-scoped question, which `host_scoped()` already flags for the reader.
            scope: None,
        });
    }
}

/// Read a flat `name: value` vars file and index every top-level key. `condition` is the
/// `when:` guarding the load, if any (only `include_vars`, a task, can carry one). Records
/// the file (canonicalised) as a dependency.
fn read_var_file(
    file: &Path,
    source: VarSource,
    condition: Option<String>,
    out: &mut Contribution,
    walk: &mut Walk,
) {
    let Some(src) = walk.cache.source(file) else {
        return;
    };
    if let Some(c) = &src.canon {
        out.deps.insert(c.clone());
    }
    let Some(nodes) = &src.nodes else {
        return;
    };
    for n in nodes.iter() {
        if let Node::Mapping { entries, .. } = n {
            for (k, v) in entries {
                if let Some(name) = k.as_str() {
                    out.defs.push(Located {
                        name: name.to_string(),
                        source,
                        span: v.span(),
                        file: file.to_path_buf(),
                        after: None,
                        condition: condition.clone(),
                        via: Vec::new(),
                        // A whole vars file is in scope wherever it is loaded.
                        scope: None,
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

/// Resolve an `include_vars:` file entry against the file dir, its `vars/`, the role
/// `vars/` and the project root, returning the first path that exists. `vars_files` no
/// longer routes through here — it uses [`crate::resolve::vars_files_candidates`], the
/// ported play-level search order.
fn resolve_var_path(entry: &str, ctx: &FileContext, fs: &dyn Fs) -> Option<PathBuf> {
    // Same list the resolver navigates by. Held apart, these two disagreed about which file
    // an `include_vars:` names, so the index read one file while go-to-definition opened
    // another (T-206).
    ctx.include_vars_bases().into_iter().map(|b| b.join(entry)).find(|p| fs.is_file(p))
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

    /// T-222: `role_names`, `inventory_file` and `environment` are set for every task
    /// (2.21.2 `varnames` run, see `injected.rs`), and the hand-typed list this rule used to
    /// read lacked all three. The seventh name is the control: the rule still fires.
    ///
    /// Consumers of `injected::provided`, and what each answers for `inventory_file`
    /// (rule 3): this rule — silent; `condition::variable_uses` — dropped, so no condition
    /// hint names it; hover (`main.rs` `variable_hover_at`) — the table's line, since no
    /// definition exists; go-to-definition — nothing, for the same reason.
    ///
    /// `role_uuid`, the fourth name the ticket listed, is *not* here: it is present only
    /// inside a role, so a play task reading it was a true positive, not a gap.
    #[test]
    fn names_ansible_sets_for_every_task_stay_silent() {
        let src = concat!(
            "- hosts: all\n  tasks:\n",
            "    - debug: { msg: \"{{ role_names }} {{ inventory_file }} {{ environment }} ",
            "{{ groups }} {{ inventory_dir }} {{ nope_missing }}\" }\n",
        );
        assert_eq!(undef(src), ["nope_missing"]);
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

        // T-066: base's defs arrived through app's meta edge, and carry it as provenance.
        let mtu = defs.iter().find(|x| x.name == "base_mtu").unwrap();
        assert_eq!(mtu.via.len(), 1);
        assert!(mtu.via[0].0.ends_with("roles/app/meta/main.yml"));
    }

    /// T-066: a role the playbook names directly needs no breadcrumb — the route is the
    /// line the user wrote.
    #[test]
    fn directly_named_role_defs_carry_no_via() {
        let d = std::env::temp_dir().join("ansible-lsp-t066-direct");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write(&d, "roles/app/defaults/main.yml", "app_port: 80\n");
        write(&d, "roles/app/tasks/main.yml", "- debug: { msg: hi }\n");
        let play = d.join("play.yml");
        std::fs::write(&play, "- hosts: all\n  roles: [app]\n").unwrap();

        let nodes = Document::new(std::fs::read_to_string(&play).unwrap()).parse().unwrap();
        let defs = definitions(&play, &nodes);
        assert!(defs.iter().find(|x| x.name == "app_port").unwrap().via.is_empty());
    }

    /// T-066: a role reachable both directly and as a meta dependency gets its breadcrumb
    /// from whichever route comes first in listed order — because that's the copy Ansible
    /// actually executes (the later copy is skipped per host: play_iterator.py "role has
    /// already run"; live-proven 2026-08-02, ok=2 with the direct copy dead).
    #[test]
    fn dual_route_via_follows_listed_order_like_ansible_dedup() {
        let d = std::env::temp_dir().join("ansible-lsp-t066-dual");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write(&d, "roles/app/meta/main.yml", "dependencies:\n  - base\n");
        write(&d, "roles/app/tasks/main.yml", "- debug: { msg: hi }\n");
        write(&d, "roles/base/defaults/main.yml", "base_mtu: 1500\n");
        write(&d, "roles/base/tasks/main.yml", "- debug: { msg: hi }\n");

        let via_of = |roles: &str| {
            let play = d.join("play.yml");
            std::fs::write(&play, format!("- hosts: all\n  roles: {roles}\n")).unwrap();
            let nodes = Document::new(std::fs::read_to_string(&play).unwrap()).parse().unwrap();
            let defs = definitions(&play, &nodes);
            defs.iter().find(|x| x.name == "base_mtu").unwrap().via.clone()
        };
        // [app, base]: base executes as app's dependency; the direct listing is the
        // skipped copy — breadcrumb present.
        let via = via_of("[app, base]");
        assert_eq!(via.len(), 1);
        assert!(via[0].0.ends_with("roles/app/meta/main.yml"));
        // [base, app]: base executes directly; the dep copy is skipped — no breadcrumb.
        assert!(via_of("[base, app]").is_empty());
    }

    /// T-066: on a transitive chain a -> b -> c, c's defs carry the full chain outermost
    /// first — a's meta (naming b), then b's meta (naming c) — the order a reader follows
    /// the links from the playbook.
    #[test]
    fn transitive_dep_via_carries_the_full_chain_in_reading_order() {
        let d = std::env::temp_dir().join("ansible-lsp-t066-chain");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write(&d, "roles/a/meta/main.yml", "dependencies:\n  - b\n");
        write(&d, "roles/a/tasks/main.yml", "- debug: { msg: hi }\n");
        write(&d, "roles/b/meta/main.yml", "dependencies:\n  - c\n");
        write(&d, "roles/b/tasks/main.yml", "- debug: { msg: hi }\n");
        write(&d, "roles/c/defaults/main.yml", "c_var: 3\n");
        write(&d, "roles/c/tasks/main.yml", "- debug: { msg: hi }\n");
        let play = d.join("play.yml");
        std::fs::write(&play, "- hosts: all\n  roles: [a]\n").unwrap();

        let nodes = Document::new(std::fs::read_to_string(&play).unwrap()).parse().unwrap();
        let defs = definitions(&play, &nodes);
        let c = defs.iter().find(|x| x.name == "c_var").unwrap();
        assert_eq!(c.via.len(), 2);
        assert!(c.via[0].0.ends_with("roles/a/meta/main.yml"), "got {}", c.via[0].0.display());
        assert!(c.via[1].0.ends_with("roles/b/meta/main.yml"), "got {}", c.via[1].0.display());
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
        // The alignment is the test: a block scalar whose body holds a multi-byte character,
        // so byte offsets and column counts part ways. Written out rather than escaped,
        // because escaping hides exactly the thing being guarded.
        let src = r#"
            - hosts: all
              tasks:
                - debug:
                    msg: |
                      aa
                      ═══{{ zzz_und }}
"#;
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

    /// T-177. `add_host` is the second module whose argument keys are definitions, and the
    /// list of keys that are *not* is upstream's `special_args`, never the module's alias
    /// list — the docs give the wrong four.
    ///
    /// Measured on 2.21.2 by diffing each spelling's created host against a `name:`-only
    /// baseline. All five spellings created their host and group, so the aliases do work;
    /// only `host:` and `group:` also left a variable of that name behind.
    #[test]
    fn add_host_argument_keys_are_definitions_but_its_own_parameters_are_not() {
        let i = idx(concat!(
            "- hosts: localhost\n",
            "  tasks:\n",
            "    - ansible.builtin.add_host:\n",
            "        name: newhost\n",
            "        groups: created\n",
            "        pod_namespace: my-namespace\n",
            "    - add_host:\n",
            "        hostname: alt\n",
            "        groupname: alt_group\n",
            "    - add_host:\n",
            "        host: leaks\n",
            "        group: leaks_too\n",
        ));
        let src = |n: &str| i.get(n).first().map(|d| d.source);
        assert_eq!(src("pod_namespace"), Some(VarSource::AddHost));
        for consumed in ["name", "hostname", "groups", "groupname"] {
            assert!(
                i.get(consumed).is_empty(),
                "`{consumed}` is consumed as a parameter and defines nothing"
            );
        }
        // The measured surprise, and the reason the alias list is the wrong source: these
        // two are read as aliases *and* fall through into the created host's variables.
        assert_eq!(src("host"), Some(VarSource::AddHost), "`host:` leaks a variable");
        assert_eq!(src("group"), Some(VarSource::AddHost), "`group:` leaks a variable");
    }

    /// T-170's control, re-asserted from this side: a templated `add_host` key defines
    /// nothing. Measured — the host variable is really named `{{ dyn }}`, which no
    /// expression can reference, so filing the literal would index an unreachable name.
    #[test]
    fn a_templated_add_host_key_defines_nothing() {
        let i = idx(concat!(
            "- hosts: localhost\n",
            "  tasks:\n",
            "    - add_host:\n",
            "        name: h\n",
            "        \"{{ dyn }}\": v\n",
            "        literal_beside_it: v\n",
        ));
        assert!(i.get("dyn").is_empty());
        assert!(i.get("{{ dyn }}").is_empty());
        // The control: the same task's literal key is indexed, so the assertion above is
        // about the braces and not about the task being skipped wholesale.
        assert_eq!(i.get("literal_beside_it").len(), 1);
    }

    /// A known miss, pinned so it is a decision rather than an accident.
    ///
    /// Measured on 2.21.2: `add_host: name=ff01 groups=ffgroup ff_var=FROM_FREEFORM` creates
    /// the host and the next play reads `ff_var` back. We index nothing from it, so a use of
    /// `ff_var` elsewhere is still reported undefined — the same false positive T-177 exists
    /// to remove, surviving in the other spelling. The fix is `parse_kv`/shlex semantics on
    /// `_raw_params`, which is T-046's whole subject; guessing at it here is how a splitter
    /// that disagrees with ansible gets shipped.
    ///
    /// When T-046 lands, this test should flip to asserting `ff_var` IS indexed.
    #[test]
    fn the_free_form_add_host_spelling_is_a_known_miss() {
        let i = idx(concat!(
            "- hosts: localhost\n",
            "  tasks:\n",
            "    - add_host: name=ff01 groups=ffgroup ff_var=FROM_FREEFORM\n",
        ));
        assert!(i.get("ff_var").is_empty(), "T-046 would make this a definition");
        // The control: the mapping spelling of the same task is indexed, so this is about
        // the free-form args and not about `add_host` handling having gone away.
        let m = idx(concat!(
            "- hosts: localhost\n",
            "  tasks:\n",
            "    - add_host:\n",
            "        name: ff01\n",
            "        ff_var: FROM_MAPPING\n",
        ));
        assert_eq!(m.get("ff_var").len(), 1);
    }

    /// The T-177 reproduction. It runs clean on 2.21.2 — measured — while we reported
    /// `pod_namespace` undefined, which is the failure this project exists to avoid.
    #[test]
    fn a_variable_defined_by_add_host_is_not_undefined_in_a_later_play() {
        let play = |defines: &str| {
            format!(
                concat!(
                    "- hosts: localhost\n",
                    "  gather_facts: false\n",
                    "  tasks:\n",
                    "    - ansible.builtin.add_host:\n",
                    "        name: \"{{{{ item }}}}\"\n",
                    "        groups: k8s_pods\n",
                    "{}",
                    "      loop: [a, b]\n",
                    "\n",
                    "- hosts: k8s_pods\n",
                    "  gather_facts: false\n",
                    "  tasks:\n",
                    "    - debug:\n",
                    "        msg: \"ns={{{{ pod_namespace }}}}\"\n",
                ),
                defines
            )
        };
        assert!(undef(&play("        pod_namespace: my-namespace\n")).is_empty());
        // The control. Without that one line the read is genuinely undefined, so the
        // assertion above is the indexing working and not the rule having gone quiet.
        assert_eq!(undef(&play("")), ["pod_namespace"]);
    }

    /// Ordering is not the axis here, and the probe says so rather than the reading.
    ///
    /// Measured on 2.21.2: a read on the *calling* host is `UNDEF` both before and after
    /// the `add_host` task — the variable never reaches the host that ran it, only the
    /// hosts it created. So `add_host` stays out of [`Located::ordered_before`], where a
    /// `set_fact`-style rule would have made a same-file earlier use a false positive.
    #[test]
    fn a_use_before_the_add_host_task_is_still_exempt() {
        let src = concat!(
            "- hosts: localhost\n",
            "  tasks:\n",
            "    - debug: { msg: \"{{ later_added }}\" }\n",
            "    - add_host:\n",
            "        name: h\n",
            "        later_added: 1\n",
        );
        assert!(undef(src).is_empty());
    }

    /// The rungs measured for T-177, asserted so a later edit to `precedence` has to break
    /// a named claim rather than a number. Level 8 — bracketed on both sides in one run,
    /// with `only_group`/`only_addhost`/`only_play`/`only_hostvars` alive as controls.
    #[test]
    fn add_host_sits_above_group_vars_and_below_host_vars() {
        use VarSource::*;
        assert!(AddHost.precedence() > GroupVars.precedence(), "beat playbook group_vars");
        assert!(AddHost.precedence() < HostVars.precedence(), "lost to host_vars");
        assert!(AddHost.precedence() < PlayVars.precedence(), "lost to play vars");
        assert!(AddHost.precedence() < SetFact.precedence(), "lost to set_fact");
        // Scope, settled by the same run: `preexisting`, already in the group and in the
        // play, read the variable as UNDEF. Only the created hosts get it, and they are
        // named at runtime — so this can never be asserted for a given host.
        assert!(AddHost.host_scoped());
    }

    /// T-169, the walk consumer: a template in a mapping key is a variable use at the two
    /// places Ansible renders one. Both measured on 2.21.2 — `set_fact` with
    /// `result_name: my_result` created the fact `my_result`, and `set_stats` reported the
    /// custom stat as `dynamic_stat`.
    #[test]
    fn a_template_in_a_rendered_key_is_a_use() {
        let names = |src: &str| -> Vec<String> {
            let nodes = Document::new(src.to_string()).parse().expect("valid yaml");
            uses(&nodes).into_iter().map(|u| u.name).collect()
        };
        assert_eq!(
            names("- hosts: all\n  tasks:\n    - set_fact:\n        \"{{ result_name }}\": true\n"),
            ["result_name"]
        );
        // The FQCN spelling is the same action.
        assert_eq!(
            names("- hosts: all\n  tasks:\n    - ansible.builtin.set_fact:\n        \"pre_{{ n }}\": 1\n"),
            ["n"]
        );
        // set_stats renders the keys of `data:` only — one level in, not its own args.
        assert_eq!(
            names("- hosts: all\n  tasks:\n    - set_stats:\n        data:\n          \"{{ k }}_stat\": 1\n"),
            ["k"]
        );
        // The span is the name inside the braces, so hover highlights the name.
        let src = "- hosts: all\n  tasks:\n    - set_fact:\n        \"{{ result_name }}\": true\n";
        let nodes = Document::new(src.to_string()).parse().unwrap();
        assert_eq!(uses(&nodes)[0].span.slice(src), "result_name");
    }

    /// The other half, and the reason this rule is two sites rather than "every key":
    /// everywhere else the braces are not a use. Measured — an unknown module parameter
    /// is fatal (`Unsupported parameters for … debug module: {{ argname }}`, so nothing
    /// was rendered) and a key nested in ordinary data keeps its braces (`set_fact` of a
    /// dict whose key was `{{ k }}` printed `keys=['{{ k }}']`). A use invented here would
    /// hover a name Ansible never resolves.
    #[test]
    fn a_template_in_a_literal_key_is_not_a_use() {
        let names = |src: &str| -> Vec<String> {
            let nodes = Document::new(src.to_string()).parse().expect("valid yaml");
            uses(&nodes).into_iter().map(|u| u.name).collect()
        };
        // An arbitrary module's arg key.
        assert!(names("- hosts: all\n  tasks:\n    - debug:\n        \"{{ argname }}\": x\n").is_empty());
        // A key nested inside a fact's value — data, not an arg name.
        assert!(names(
            "- hosts: all\n  tasks:\n    - set_fact:\n        outer:\n          \"{{ k }}\": 1\n"
        )
        .is_empty());
        // set_stats' own arg names are literal; only `data:`'s children render.
        assert!(names(
            "- hosts: all\n  tasks:\n    - set_stats:\n        \"{{ opt }}\": true\n"
        )
        .is_empty());
        // A fact *named* `set_fact` must not open a second templated level.
        assert!(names(
            "- hosts: all\n  tasks:\n    - set_fact:\n        set_fact:\n          \"{{ k }}\": 1\n"
        )
        .is_empty());
        // `add_host` is the trap: it takes arbitrary keys as host vars, so it *looks* like
        // a third rendering site. It is not — measured, the host var is really named
        // `{{ k }}` and the intended name is never set. Claiming a use here would point
        // at a definition for a variable that does not exist (its own ticket, T-170).
        assert!(names(
            "- hosts: all\n  tasks:\n    - add_host:\n        name: h\n        \"{{ k }}\": v\n"
        )
        .is_empty());
        // Values are untouched by all of this — the control that keeps the walk honest.
        assert_eq!(
            names("- hosts: all\n  tasks:\n    - debug:\n        msg: \"{{ shown }}\"\n"),
            ["shown"]
        );
    }

    /// T-169, the `undefined_uses` consumer: a name used in a rendered key is checked like
    /// any other, with the defined spelling as the control.
    #[test]
    fn undefined_uses_sees_a_name_inside_a_rendered_key() {
        assert_eq!(
            undef("- hosts: all\n  tasks:\n    - set_fact:\n        \"{{ result_name }}\": true\n"),
            ["result_name"]
        );
        // Defined in the play: silent. Same line, opposite verdict.
        assert!(undef(concat!(
            "- hosts: all\n  vars:\n    result_name: my_result\n  tasks:\n",
            "    - set_fact:\n        \"{{ result_name }}\": true\n",
        ))
        .is_empty());
        // And a literal key never was a use, so it cannot become an undefined one.
        assert!(undef("- hosts: all\n  tasks:\n    - debug:\n        \"{{ argname }}\": x\n").is_empty());
    }

    /// T-104: which sources a `hostvars[...]` read can see. This is the measured table on
    /// `VarSource`, asserted here so a source added later has to declare a side. Every row
    /// was run cross-host on 2.21.2 (web02 reading web01), not read off the precedence
    /// list — reading it would have put `include_vars` on the wrong side.
    #[test]
    fn hostvars_visibility_matches_what_was_measured() {
        use VarSource::*;
        for s in [GroupVarsAll, GroupVars, HostVars, Inventory, IncludeVars, SetFact, Register, AddHost]
        {
            assert!(s.visible_to_hostvars(), "{s:?} was measured VISIBLE");
        }
        for s in [
            PlayVars, BlockVars, TaskVars, VarsFiles, RoleDefaults, RoleVars, RoleParams,
            RoleEntryVars,
        ] {
            assert!(!s.visible_to_hostvars(), "{s:?} was measured invisible");
        }
        // The pair that makes this a measurement and not a precedence rule: both load a
        // YAML file of variables, and only one survives into the host's own storage.
        assert!(IncludeVars.visible_to_hostvars());
        assert!(!VarsFiles.visible_to_hostvars());
    }

    /// T-104: `undefined_uses` says **nothing** about a `hostvars[...]` read, in either
    /// direction. Not a gap — a retraction, because the claim was not sound.
    ///
    /// The obvious rule is "every definition I can see is play-scoped, so this is always
    /// undefined". It is wrong: `hostvars` is answered mostly by inventory, which we do
    /// not parse (T-062). Measured both ways round —
    ///
    /// - found nothing: 37 corpus warnings, all 37 inventory host vars.
    /// - found only play-scoped: a name in play `vars:` *and* in inventory reads fine
    ///   through hostvars (measured: returns the inventory value), so the warning fires
    ///   on working code.
    ///
    /// The navigation half is unaffected and still applies `visible_to_hostvars`, which
    /// is sound for it: whatever inventory holds, the play var is not what this read
    /// returns, so hover must not offer it.
    #[test]
    fn a_hostvars_read_is_never_reported_undefined() {
        // Defined only in play vars — the case that looks provable and is not.
        assert!(undef(concat!(
            "- hosts: all\n  vars:\n    play_scoped: 8080\n  tasks:\n",
            "    - debug: { msg: \"{{ hostvars['web01'].play_scoped }}\" }\n",
        ))
        .is_empty());
        // Defined nowhere we can see — the 37-false-positive shape.
        assert!(undef(concat!(
            "- hosts: all\n  tasks:\n",
            "    - debug: { msg: \"{{ hostvars[item].infiniband_ip }}\" }\n",
        ))
        .is_empty());
        // A fact is host storage, so hostvars genuinely sees it.
        assert!(undef(concat!(
            "- hosts: all\n  tasks:\n",
            "    - set_fact: { gathered: 1 }\n",
            "    - debug: { msg: \"{{ hostvars['web01'].gathered }}\" }\n",
        ))
        .is_empty());
        // THE CONTROL, and the reason this test is not vacuous: the ordinary read of the
        // very same undefined name still warns. Silence above is the hostvars rule, not
        // the check being asleep.
        assert_eq!(
            undef("- hosts: all\n  tasks:\n    - debug: { msg: \"{{ nowhere_at_all }}\" }\n"),
            ["nowhere_at_all"]
        );
        // The use is still extracted and still marked — hover and go-to-definition need
        // both, and T-062 will need them to make the diagnostic sound.
        let src = "- hosts: all\n  vars:\n    play_scoped: 8080\n  tasks:\n    - debug: { msg: \"{{ hostvars['w'].play_scoped }}\" }\n";
        let nodes = Document::new(src.to_string()).parse().unwrap();
        let u = uses(&nodes).into_iter().find(|u| u.through_hostvars).expect("still a use");
        assert_eq!(u.name, "play_scoped");
    }

    /// `set_stats` renders its keys exactly like `set_fact` (T-169) and is otherwise
    /// nothing like it: the names it creates are run statistics handed to callback
    /// plugins, and no expression can read one back — measured, `{{ from_stat }}` is
    /// undefined in the very play that set it, while the same name is reported under
    /// CUSTOM STATS. So its keys are variable *uses* and never definitions. The symmetry
    /// is the trap: indexing both plugins would file a definition nothing can reference,
    /// and would silence a correct `var-undefined` on anyone who expected otherwise.
    #[test]
    fn set_stats_renders_keys_but_defines_no_variables() {
        let i = idx(concat!(
            "- hosts: all\n  tasks:\n",
            "    - set_stats:\n        data:\n          from_stat: 222\n",
            "    - set_fact:\n        from_fact: 111\n",
        ));
        assert!(i.get("from_stat").is_empty());
        assert!(i.get("data").is_empty());
        // The control: the plugin beside it, one line down, does define one.
        assert_eq!(i.get("from_fact").first().map(|d| d.source), Some(VarSource::SetFact));
        // And so a later read of the stat is correctly undefined, as Ansible has it.
        assert_eq!(
            undef(concat!(
                "- hosts: all\n  tasks:\n",
                "    - set_stats:\n        data:\n          from_stat: 222\n",
                "    - debug: { msg: \"{{ from_stat }}\" }\n",
            )),
            ["from_stat"]
        );
    }

    /// The two spellings the corpus actually writes — both of the two templated `set_fact`
    /// keys in 753 files. Scanning keys put these names in front of `undefined_uses` for
    /// the first time, so this is the shape a new false "undefined" would have taken: a
    /// `loop:` variable in the key, and a `default(...)` fallback. The bare name is the
    /// control that proves the silence is the exemptions working, not the walk missing.
    #[test]
    fn the_templated_key_spellings_the_corpus_uses_stay_silent() {
        assert!(undef(concat!(
            "- hosts: all\n  tasks:\n",
            "    - set_fact:\n        \"{{ item.key }}\": \"{{ item.value }}\"\n",
            "      loop: [1]\n",
        ))
        .is_empty());
        assert!(undef(concat!(
            "- hosts: all\n  tasks:\n",
            "    - set_fact:\n        \"{{ picked | default('fallback') }}\": x\n",
        ))
        .is_empty());
        assert_eq!(
            undef("- hosts: all\n  tasks:\n    - set_fact:\n        \"{{ picked }}\": x\n"),
            ["picked"]
        );
    }

    /// T-169 fault 2: the fact a templated key creates is named only after rendering, so
    /// filing the literal put `{{ result_name }}` in the index — a name no expression can
    /// reference. A literal key is the control and still indexes.
    #[test]
    fn a_templated_set_fact_key_defines_nothing_while_a_literal_one_still_does() {
        let i = idx(concat!(
            "- hosts: all\n  vars:\n    result_name: my_result\n  tasks:\n",
            "    - set_fact:\n        \"{{ result_name }}\": true\n        plain_fact: 1\n",
        ));
        let names: Vec<&str> = i
            .defs()
            .iter()
            .filter(|d| d.source == VarSource::SetFact)
            .map(|d| d.name.as_str())
            .collect();
        assert_eq!(names, ["plain_fact"]);
        // The rendered name (`my_result`) is knowingly not indexed either — that needs the
        // template evaluated, which is T-034's.
        assert!(i.get("my_result").is_empty());
    }

    /// T-100: a role param is a variable definition, at precedence 20. Live-verified —
    /// the role read `{{ tasks_from }}` and got `alternate.yml`.
    #[test]
    fn role_params_are_indexed_as_variables() {
        let src = concat!(
            "- hosts: all\n",
            "  roles:\n",
            "    - role: web\n",
            "      tasks_from: alternate.yml\n",
            "      port_count: 4\n",
        );
        let i = index(&ast::build(&Document::new(src.to_string()).parse().unwrap()));
        assert_eq!(i.get("port_count").first().map(|d| d.source), Some(VarSource::RoleParams));
        // The span is the value, so go-to-definition lands where the other sources land.
        assert_eq!(i.get("tasks_from")[0].span.slice(src), "alternate.yml");
        assert_eq!(VarSource::RoleParams.precedence(), 20);
        // A keyword on the entry is a setting, not a variable.
        assert!(i.get("role").is_empty());
    }

    /// Both ways of passing a value on one entry, and which wins. Live-verified on
    /// 2.21.2: with `vars: {myport: 90}` and `myport: 80` on the same entry the role sees
    /// **80** — the param — in either write order. Reading `get_vars()` alone suggests the
    /// opposite (`self.vars` is combined last), which is why this is measured.
    #[test]
    fn a_role_param_outranks_the_entrys_vars_for_the_same_name() {
        let winner = |src: &str| {
            let nodes = crate::parse::Document::new(src.to_string()).parse().unwrap();
            let i = index(&ast::build(&nodes));
            i.get("myport").iter().map(|d| d.source).max_by_key(|s| s.precedence())
        };
        for src in [
            "- hosts: all\n  roles:\n    - role: web\n      vars: {myport: 90}\n      myport: 80\n",
            "- hosts: all\n  roles:\n    - role: web\n      myport: 80\n      vars: {myport: 90}\n",
        ] {
            assert_eq!(winner(src), Some(VarSource::RoleParams), "write order must not decide");
        }
    }

    /// The trap in the obvious example: `port` is one of the 28 keys
    /// `RoleInclude.fattributes` claims, so `port: 80` on an entry is the *connection
    /// port* and never a variable at all. Live-verified — the role sees `port` undefined
    /// unless a `vars:` supplies it. Only a name outside the legal set is a param.
    #[test]
    fn a_keyword_named_entry_key_is_not_a_variable() {
        let src = "- hosts: all\n  roles:\n    - role: web\n      vars: {port: 90}\n      port: 80\n";
        let i = index(&ast::build(&crate::parse::Document::new(src.to_string()).parse().unwrap()));
        let sources: Vec<_> = i.get("port").iter().map(|d| d.source).collect();
        assert_eq!(sources, [VarSource::RoleEntryVars], "port: 80 is a keyword, not a param");
    }

    /// Entry `vars:` beat the role's own `vars/main.yml` — same bucket, combined after it.
    #[test]
    fn entry_vars_outrank_the_roles_vars_main() {
        assert!(
            VarSource::RoleEntryVars.precedence() > VarSource::RoleDefaults.precedence()
                && VarSource::RoleEntryVars.precedence() >= VarSource::RoleVars.precedence()
                && VarSource::RoleEntryVars.precedence() < VarSource::RoleParams.precedence()
        );
    }

    /// The ordinary spelling, which is the one that was missing: `vars:` on a `roles:`
    /// entry is a legal keyword, so it never lands in `params` and needs its own read.
    #[test]
    fn entry_vars_are_indexed() {
        let src = "- hosts: all\n  roles:\n    - role: web\n      vars: {port: 80}\n";
        let i = index(&ast::build(&crate::parse::Document::new(src.to_string()).parse().unwrap()));
        assert_eq!(i.get("port").first().map(|d| d.source), Some(VarSource::RoleEntryVars));
        assert_eq!(i.get("port")[0].span.slice(src), "80");
    }

    /// A role param is in scope for the rest of its own entry and out of scope in the
    /// play's tasks — both measured on 2.21.2. The index is per-file and flat, so without
    /// the entry check one of the two has to be wrong: either a false `var-undefined` on a
    /// param built from another param, or silence on a use that really does fail at run
    /// time. Both are asserted here so neither can be traded for the other.
    #[test]
    fn entry_scoped_names_are_visible_in_the_entry_and_not_in_the_plays_tasks() {
        let d = std::env::temp_dir().join("ansible-lsp-entry-scope");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let flagged = |src: &str| {
            let p = d.join("site.yml");
            std::fs::write(&p, src).unwrap();
            let nodes = crate::parse::Document::new(src.to_string()).parse().unwrap();
            let mut n: Vec<String> =
                undefined_uses(&p, &nodes, src).into_iter().map(|u| u.name).collect();
            n.sort();
            n
        };
        // Inside the entry: one param built from another. Silent — it works.
        assert!(
            flagged(concat!(
                "- hosts: all\n",
                "  roles:\n",
                "    - role: provisioner\n",
                "      app_env: staging\n",
                "      app_config_dir: /etc/provisioner/{{ app_env }}\n",
            ))
            .is_empty()
        );
        // The entry's `vars:` reach the rest of the entry too.
        assert!(
            flagged(concat!(
                "- hosts: all\n",
                "  roles:\n",
                "    - role: provisioner\n",
                "      vars: {app_env: staging}\n",
                "      app_config_dir: /etc/provisioner/{{ app_env }}\n",
            ))
            .is_empty()
        );
        // In the play's tasks: not in scope, and this really does fail at run time.
        assert_eq!(
            flagged(concat!(
                "- hosts: all\n",
                "  roles:\n",
                "    - role: provisioner\n",
                "      app_env: staging\n",
                "  tasks:\n",
                "    - debug: {msg: \"{{ app_env }}\"}\n",
            )),
            ["app_env"]
        );
        // A play `vars:` is not entry-scoped, so the same use is fine.
        assert!(
            flagged(concat!(
                "- hosts: all\n",
                "  vars: {app_env: staging}\n",
                "  roles:\n",
                "    - role: provisioner\n",
                "  tasks:\n",
                "    - debug: {msg: \"{{ app_env }}\"}\n",
            ))
            .is_empty()
        );
    }

    /// The two reasons a use is flagged are different advice, so they must not be mixed
    /// up: one says "add a definition", the other says "the definition you already wrote
    /// does not reach here".
    #[test]
    fn out_of_scope_is_distinguished_from_never_defined() {
        let d = std::env::temp_dir().join("ansible-lsp-oos");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let flags = |src: &str| {
            let p = d.join("site.yml");
            std::fs::write(&p, src).unwrap();
            let nodes = crate::parse::Document::new(src.to_string()).parse().unwrap();
            undefined_uses(&p, &nodes, src)
                .into_iter()
                .map(|u| (u.name, u.defined_out_of_scope))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            flags(concat!(
                "- hosts: all\n  roles:\n    - role: r\n      app_env: staging\n",
                "  tasks:\n    - debug: {msg: \"{{ app_env }}\"}\n",
            )),
            [("app_env".to_string(), true)]
        );
        assert_eq!(
            flags("- hosts: all\n  tasks:\n    - debug: {msg: \"{{ nowhere_at_all }}\"}\n"),
            [("nowhere_at_all".to_string(), false)]
        );
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
        let nodes = crate::parse::Document::new(src.to_string()).parse().unwrap();
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
        let nodes = crate::parse::Document::new(src.to_string()).parse().unwrap();
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

    /// The demo's mock dynamic inventory does what its header says it does.
    ///
    /// A demo label is a claim (CLAUDE.md rule 4), and this one is checkable in a way its
    /// neighbour `inventory-dynamic.yml` is not: that file names `amazon.aws.aws_ec2`, so
    /// nobody without an AWS account can confirm Ansible would run it. This one needs no
    /// credentials and no network —
    ///
    ///     ansible-inventory -i demo/inventory-dynamic.sh --list
    ///
    /// prints `mock01` with `mock_dynamic_var`, which is what makes "we decline to run it"
    /// a measured refusal rather than an assertion about a file nobody can execute.
    ///
    /// Asserted against the real demo file, not a copy: a copy would keep passing after
    /// someone cleared the execute bit or dropped the shebang, which are the two things the
    /// classification actually turns on.
    #[test]
    #[cfg(unix)]
    fn the_demo_mock_dynamic_inventory_is_declined_and_defines_nothing() {
        let sh = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../demo/inventory-dynamic.sh");
        assert!(sh.is_file(), "demo fixture missing: {}", sh.display());
        use crate::fs::Fs as _;
        assert!(
            crate::fs::StdFs.is_executable(&sh),
            "the execute bit is the fixture — without it Ansible's ini plugin reads this file"
        );
        let text = std::fs::read_to_string(&sh).unwrap();
        assert!(text.starts_with("#!"), "the shebang is what keeps it dynamic despite its text");
        assert!(
            text.contains("mock_dynamic_var"),
            "the script must actually emit a variable, or 'we never read it' proves nothing"
        );

        let cache = ScanCache::default();
        let nodes = Document::new(text.clone()).parse().unwrap_or_default();
        assert_eq!(
            crate::inventory::classify(&sh, &text, &nodes, &cache),
            crate::inventory::Kind::Dynamic
        );

        // Named as the inventory, it contributes nothing and is *recorded* as declined —
        // the two halves of "handled". Silence alone would be indistinguishable from an
        // inventory we read that happened to be empty.
        let cache = ScanCache::default().with_inventory(vec![sh.clone()]);
        let play = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../demo/playbook.yml");
        let declined = declined_inventories(&play, &cache);
        assert_eq!(declined.len(), 1, "the demo script must be recorded: {declined:?}");
        assert!(declined[0].ends_with("inventory-dynamic.sh"));

        let pnodes = Document::new(std::fs::read_to_string(&play).unwrap()).parse().unwrap();
        let defs = definitions_with_deps_in(&play, &pnodes, &cache).0;
        for name in ["mock_dynamic_var", "mock_region"] {
            assert!(!defs.iter().any(|d| d.name == name), "{name} was harvested from a script");
        }
    }

    /// A declined source is *recorded*, not merely skipped — the two states that must not
    /// look alike are "read it, there are no hosts" and "did not read it, hosts unknown".
    ///
    /// Both produce zero definitions, which is why skipping silently was not enough: a
    /// host-existence rule reading only `defs` cannot tell a genuinely empty inventory from a
    /// cloud one we declined, and would report every `hostvars[...]` in the second case as an
    /// unknown host. The empty-inventory row is the control that makes this a distinction
    /// rather than a restatement of "dynamic inventories define nothing".
    #[test]
    fn a_declined_inventory_is_recorded_and_an_empty_one_is_not() {
        let d = std::env::temp_dir().join("ansible-lsp-t062-declined");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let play = d.join("play.yml");
        std::fs::write(&play, "- hosts: all\n  tasks: []\n").unwrap();
        let nodes = Document::new(std::fs::read_to_string(&play).unwrap()).parse().unwrap();

        // A plugin config: nothing defined, and the reason is recorded.
        write(&d, "ansible.cfg", "[defaults]\ninventory = dyn.yml\n");
        write(&d, "dyn.yml", "plugin: amazon.aws.aws_ec2\nregions:\n  - us-east-1\n");
        let cache = ScanCache::default();
        let declined = declined_inventories(&play, &cache);
        assert_eq!(declined.len(), 1, "the plugin config must be recorded: {declined:?}");
        assert!(declined[0].ends_with("dyn.yml"));

        // An inventory that is genuinely empty also defines nothing — and must NOT be
        // recorded, or the caveat would fire everywhere and mean nothing.
        write(&d, "ansible.cfg", "[defaults]\ninventory = empty.ini\n");
        write(&d, "empty.ini", "");
        let cache = ScanCache::default();
        assert!(
            declined_inventories(&play, &cache).is_empty(),
            "an empty inventory was read, not declined"
        );
        assert!(definitions(&play, &nodes).iter().all(|x| x.name != "plugin"));
    }

    /// A stray execute bit does not make a data file dynamic.
    ///
    /// `script.verify_file` accepts any executable, but the manager only `break`s on a plugin
    /// that **succeeds** — a failing one is caught and the next plugin tries the same file.
    /// Measured on 2.21.2: an INI inventory at mode 755 is executed, fails, and is then read
    /// as INI, resolving identically to the same file at 644. Treating the bit alone as
    /// "dynamic" silently dropped the whole inventory on any checkout where modes are noise —
    /// a bind mount, exFAT, someone's `chmod -R 755` — and every name in it read as undefined.
    ///
    /// This is one half of a pair. The `dyn.sh` fixture in the wiring test above is the other:
    /// it carries a `[web:vars]` section precisely so the content sniff *would* claim it, and
    /// its `#!` is what keeps it dynamic. Neither test alone pins the rule.
    #[test]
    #[cfg(unix)]
    fn a_stray_execute_bit_does_not_make_a_data_file_dynamic() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join("ansible-lsp-t062-execbit");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write(&d, "ansible.cfg", "[defaults]\ninventory = hosts.ini\n");
        write(&d, "hosts.ini", "[web:vars]\nexec_probe: 1\nfrom_data_file=1\n");
        let play = d.join("play.yml");
        std::fs::write(&play, "- hosts: web\n  tasks: []\n").unwrap();
        let nodes = Document::new(std::fs::read_to_string(&play).unwrap()).parse().unwrap();

        let mode = |m: u32| {
            std::fs::set_permissions(d.join("hosts.ini"), std::fs::Permissions::from_mode(m))
                .unwrap();
            definitions(&play, &nodes).iter().any(|x| x.name == "from_data_file")
        };
        assert!(mode(0o644), "control: the file is readable at all");
        assert!(mode(0o755), "the same file with +x is still an ini inventory");
    }

    /// Every configured source contributes its own adjacent pair, and `host_vars` counts as
    /// much as `group_vars`. Measured on 2.21.2 with this exact tree and a two-entry pathlist:
    /// `a/group_vars`, `a/host_vars` and `b/group_vars` all reach their plays.
    ///
    /// The multi-source half is what makes this worth its own test — one source proves the
    /// path is resolved, two prove it is resolved per source rather than once.
    #[test]
    fn each_inventory_source_contributes_its_own_group_vars_and_host_vars() {
        let d = std::env::temp_dir().join("ansible-lsp-t062-s4");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write(&d, "ansible.cfg", "[defaults]\ninventory = a/hosts.ini,b/hosts.ini\n");
        write(&d, "a/hosts.ini", "[web]\nnode1\n");
        write(&d, "b/hosts.ini", "[db]\nnode2\n");
        write(&d, "a/group_vars/web.yml", "from_a_group: 1\n");
        write(&d, "a/host_vars/node1.yml", "from_a_host: 1\n");
        write(&d, "b/group_vars/db.yml", "from_b_group: 1\n");
        let play = d.join("play.yml");
        std::fs::write(&play, "- hosts: web\n  tasks: []\n").unwrap();
        let nodes = Document::new(std::fs::read_to_string(&play).unwrap()).parse().unwrap();
        let defs = definitions(&play, &nodes);
        for name in ["from_a_group", "from_a_host", "from_b_group"] {
            assert!(defs.iter().any(|x| x.name == name), "{name} missing: {:?}", names_of(&defs));
        }
    }

    /// [`inventory_hosts`] directly, including the shapes the editor-level tests never reach:
    /// a **directory** source expanded to several files, and two sources unioned.
    ///
    /// The `None` cases are the ones worth having a test each for. Every escape on the
    /// `unknown-host` rule is one of them, so a change that quietly turned an unknowable list
    /// into an empty one would convert all of them into false errors at once.
    #[test]
    fn inventory_hosts_unions_its_sources_and_admits_when_it_cannot_tell() {
        let d = std::env::temp_dir().join("ansible-lsp-t062-hosts");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write(&d, "dir/a.ini", "[web]\nweb[01:02]\n");
        write(&d, "dir/b.ini", "[db]\ndb01\n");
        write(&d, "solo.yml", "all:\n  hosts:\n    only1:\n");
        write(&d, "dyn.yml", "plugin: amazon.aws.aws_ec2\nregions: [us-east-1]\n");
        write(&d, "empty.ini", "# nothing here\n");
        let play = d.join("play.yml");
        std::fs::write(&play, "- hosts: all\n  tasks: []\n").unwrap();

        let hosts = |sources: Vec<PathBuf>| {
            inventory_hosts(&play, &ScanCache::default().with_inventory(sources))
                .map(|h| {
                    let mut v: Vec<String> = h.into_iter().collect();
                    v.sort();
                    v
                })
        };

        // A directory is one source that expands to several files, and the hosts of each
        // reach the same list — with the range expanded, which is the half a "no such host"
        // rule cannot get wrong.
        assert_eq!(hosts(vec![d.join("dir")]).unwrap(), ["db01", "web01", "web02"]);

        // Two sources union rather than the last one winning.
        assert_eq!(
            hosts(vec![d.join("dir/b.ini"), d.join("solo.yml")]).unwrap(),
            ["db01", "only1"]
        );

        // The three ways the answer is "I cannot tell", which must never read as "no hosts".
        assert!(hosts(vec![]).is_none(), "nothing resolved");
        assert!(hosts(vec![d.join("dyn.yml")]).is_none(), "a declined dynamic source");
        assert!(hosts(vec![d.join("nope.ini")]).is_none(), "a source that is not there");
        assert!(hosts(vec![d.join("empty.ini")]).is_none(), "parsed, but no host in it");

        // One unknowable source poisons the whole list: the others cannot vouch for the
        // hosts it would have contributed.
        assert!(hosts(vec![d.join("dir/b.ini"), d.join("dyn.yml")]).is_none());
    }

    /// [`created_hosts_in`] on its own, rather than only through the diagnostic that uses it.
    ///
    /// The `Some(empty)` vs `None` distinction is the whole contract and it has no other
    /// test: a file that creates no hosts and a file whose hosts cannot be named look
    /// identical to a caller that only counts.
    #[test]
    fn created_hosts_distinguishes_creating_nothing_from_not_knowing() {
        let d = std::env::temp_dir().join("ansible-lsp-t179-core");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("tasks")).unwrap();
        let play = d.join("play.yml");

        let hosts = |body: &str| {
            std::fs::write(&play, body).unwrap();
            let nodes = Document::new(body.to_string()).parse().unwrap();
            created_hosts_in(&play, &nodes, &ScanCache::default()).map(|h| {
                let mut v: Vec<String> = h.into_iter().collect();
                v.sort();
                v
            })
        };

        // Creates nothing, and says so — an empty set, not an absent one.
        assert_eq!(hosts("- hosts: all\n  tasks: []\n").unwrap(), Vec::<String>::new());

        // Every literal spelling of the name, all three measured to create their host.
        assert_eq!(hosts("- hosts: all\n  tasks:\n    - add_host:\n        name: a\n").unwrap(), ["a"]);
        assert_eq!(hosts("- hosts: all\n  tasks:\n    - add_host:\n        host: b\n").unwrap(), ["b"]);
        assert_eq!(hosts("- hosts: all\n  tasks:\n    - add_host: name=c\n").unwrap(), ["c"]);

        // A comma is part of the name, not a separator — measured, `name: \"a,b\"` creates a
        // single host called `a,b`. Splitting would invent two hosts and silence typos on both.
        assert_eq!(
            hosts("- hosts: all\n  tasks:\n    - add_host:\n        name: \"x,y\"\n").unwrap(),
            ["x,y"]
        );

        // Unknowable: a templated name with nothing to substitute from.
        assert!(hosts("- hosts: all\n  tasks:\n    - add_host:\n        name: \"{{ v }}\"\n").is_none());
        // Unknowable: no name key at all, so whatever it creates cannot be named.
        assert!(hosts("- hosts: all\n  tasks:\n    - add_host:\n        groups: g\n").is_none());

        // A task that is not add_host contributes nothing and knows nothing.
        assert_eq!(
            hosts("- hosts: all\n  tasks:\n    - set_fact:\n        name: notahost\n").unwrap(),
            Vec::<String>::new()
        );
    }

    /// `ansible_group_priority` is reported exactly where it is inert, and nowhere else.
    ///
    /// The inventory rows are the controls, and they are the whole point: measured on 2.21.2,
    /// the same key in `[alpha:vars]` **does** move the merge winner, so a rule that fired on
    /// every file would be wrong about the one place the key works. The table in
    /// [`ignored_group_priority`] records both halves.
    /// A duplicate of the key in **one** `group_vars` file: every occurrence is reported.
    ///
    /// Measured on 2.21.2: ansible warns `Found duplicate mapping key
    /// 'ansible_group_priority'`, keeps the **last** value — which survives as an ordinary
    /// variable, `ansible_group_priority: 99` — and the merge winner stays `zulu`. So both
    /// occurrences are inert, and reporting both is right rather than merely harmless: the
    /// hint says "this does nothing here", which is true of each one.
    ///
    /// Note this deliberately differs from the convention at `placement.rs:474`, where a
    /// diagnostic points at the surviving key alone. That rule is for a diagnostic about the
    /// *value*, where only the winner matters; this one is about the key being in the wrong
    /// file, which is equally true of every copy.
    #[test]
    fn group_priority_is_flagged_once_per_duplicate_in_a_vars_file() {
        let parse = |text: &str| Document::new(text.to_string()).parse().unwrap();
        let dup = concat!(
            "who: alpha\n",
            "ansible_group_priority: 10\n",
            "ansible_group_priority: 99\n",
        );
        let spans = ignored_group_priority(Path::new("/p/group_vars/alpha.yml"), &parse(dup));
        assert_eq!(spans.len(), 2, "one hint per occurrence");
        assert_ne!(spans[0].start, spans[1].start, "and they are distinct keys");

        // The control: an inventory source with the same duplicate stays quiet, because there
        // the key is consumed rather than inert. Without this the count above would pass on a
        // rule that fired everywhere.
        assert!(ignored_group_priority(Path::new("/p/hosts.ini"), &parse(dup)).is_empty());
    }

    #[test]
    fn group_priority_is_flagged_in_vars_files_and_not_in_an_inventory() {
        let parse = |text: &str| Document::new(text.to_string()).parse().unwrap();
        let key = "ansible_group_priority: 10\nwho: from_alpha\n";
        let spans = |rel: &str, text: &str| {
            ignored_group_priority(Path::new("/p").join(rel).as_path(), &parse(text))
        };

        // Inert: every shape the vars plugin loads.
        assert_eq!(spans("group_vars/alpha.yml", key).len(), 1, "group_vars file");
        assert_eq!(spans("host_vars/node1.yml", key).len(), 1, "host_vars file");
        assert_eq!(spans("group_vars/alpha/main.yml", key).len(), 1, "entity directory");
        assert_eq!(spans("group_vars/alpha", key).len(), 1, "extension-less");
        assert_eq!(spans("inv/group_vars/alpha.yml", key).len(), 1, "inventory-adjacent");

        // Honoured: an inventory source is parsed, not merged by the vars plugin. Flagging
        // these would be a false positive on the only place the key does anything.
        //
        // These two are what the *directory* half rests on, and they can fail: the text is a
        // flat top-level mapping, so nothing but the path keeps them quiet. Verified by
        // removing the guard — both go red.
        assert!(spans("hosts.ini", key).is_empty(), "ini inventory");
        assert!(spans("play.yml", key).is_empty(), "an ordinary playbook");

        // Nested is data, not a variable, so it was never a candidate for the merge slot.
        assert!(spans("group_vars/alpha.yml", "outer:\n  ansible_group_priority: 10\n").is_empty());

        // A YAML inventory is quiet for the *nesting* reason, not the path one — the key can
        // only ever sit under a group's `vars:` there, so the top-level filter has already
        // excluded it before the directory is consulted. Kept because it is the shape a user
        // writes, but it proves the line above, not the two before it: removing the directory
        // guard leaves this passing.
        assert!(spans("inv.yml", "alpha:\n  vars:\n    ansible_group_priority: 10\n").is_empty());

        // The span is the key, not the value — the key is what does nothing.
        let text = key.to_string();
        let got = spans("group_vars/alpha.yml", &text);
        assert_eq!(&text[got[0].start..got[0].end], "ansible_group_priority");

        // Written twice: legal YAML, the later value wins, and *both* keys are equally
        // inert. Reporting one would leave a live-looking copy on the line above.
        let twice = "ansible_group_priority: 10\nansible_group_priority: 20\n";
        assert_eq!(spans("group_vars/alpha.yml", twice).len(), 2);

        // A JSON-content vars file is read by the same plugin, so the key is just as dead.
        assert_eq!(spans("group_vars/alpha.json", "{\"ansible_group_priority\": 10}").len(), 1);

        // Nested one directory deeper inside an entity directory — still the vars plugin's.
        assert_eq!(spans("group_vars/alpha/deep/x.yml", key).len(), 1);

        // A directory merely *named* like one is not one; the component must match exactly.
        assert!(spans("my_group_vars/alpha.yml", key).is_empty());
        assert!(spans("group_vars_old/alpha.yml", key).is_empty());
    }

    /// An entity *directory* may carry an extension. `find_vars_files` matches the name at
    /// each extension slot and only then asks whether it is a directory, so `group_vars/web.yml/`
    /// is web's vars directory and its contents load — measured, `FROM_DIR_NAMED_YML` reaches
    /// the play. A directory named for an extension Ansible does not accept (`db.txt/`) is
    /// invisible, which is the control that makes the first half a measurement.
    ///
    /// The two rules pull opposite ways — extensions gate the *lookup*, but the recursion
    /// inside a vars directory skips any subdirectory that has one — so neither can be
    /// inferred from the other.
    #[test]
    fn a_vars_directory_may_carry_an_accepted_extension_in_its_name() {
        let d = std::env::temp_dir().join("ansible-lsp-t062-extdir");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write(&d, "group_vars/web.yml/inner.yml", "dir_with_ext: 1\n");
        write(&d, "group_vars/db.txt/inner.yml", "dir_bad_ext: 1\n");
        let play = d.join("play.yml");
        std::fs::write(&play, "- hosts: web\n  tasks: []\n").unwrap();
        let nodes = Document::new(std::fs::read_to_string(&play).unwrap()).parse().unwrap();
        let defs = definitions(&play, &nodes);
        assert!(defs.iter().any(|x| x.name == "dir_with_ext"), "{:?}", names_of(&defs));
        assert!(!defs.iter().any(|x| x.name == "dir_bad_ext"));
    }

    /// Two files in one vars directory: Ansible merges them with `combine_vars` in sorted
    /// order, so the LATER file wins. Measured on 2.21.2 — and `a.yml` is padded so its
    /// variable sits at a larger byte offset than `b.yml`'s, which is what makes this a
    /// measurement rather than a restatement of the old rule: tie-breaking on `span.start`
    /// picked `a.yml`, and a hover then stated a value no run produces.
    #[test]
    fn a_later_file_in_a_vars_directory_wins_however_the_offsets_fall() {
        let d = std::env::temp_dir().join("ansible-lsp-t062-loadorder");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write(&d, "group_vars/web/a.yml", "# padding to push this past b.yml\n# more\nwho: A\n");
        write(&d, "group_vars/web/b.yml", "who: B\n");
        let play = d.join("play.yml");
        std::fs::write(&play, "- hosts: web\n  tasks: []\n").unwrap();
        let nodes = Document::new(std::fs::read_to_string(&play).unwrap()).parse().unwrap();
        let defs = definitions(&play, &nodes);
        let cands: Vec<Located> = defs.iter().filter(|x| x.name == "who").cloned().collect();
        assert_eq!(cands.len(), 2, "both files load: {cands:?}");
        let win = effective(&cands).expect("a winner");
        assert_eq!(win.file.file_name().unwrap(), "b.yml");
    }

    /// Exactly which files a `group_vars/` directory contributes, both halves asserted.
    ///
    /// Every row was run on 2.21.2 against this tree and read back through a `debug` task.
    /// The negatives carry the weight: the old reader filtered extensions at the top level
    /// but applied *no* filter inside an entity directory, so `.hidden.yml`, `c.yml~` and
    /// `d.txt` all became definitions — and an invented name is one `var-undefined` then
    /// stops reporting. It also never descended, and never looked at `.json`, so two real
    /// sources went missing and produced the opposite failure.
    #[test]
    fn group_vars_reads_the_files_ansible_reads_and_no_others() {
        let d = std::env::temp_dir().join("ansible-lsp-t062-gvsurface");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write(&d, "group_vars/all.json", "{\"json_top\": 1}\n");
        write(&d, "group_vars/all.txt", "txt_top: 1\n");
        write(&d, "group_vars/web/a.yml", "dvar: 1\n");
        write(&d, "group_vars/web/sub/b.yml", "dvar2: 1\n");
        write(&d, "group_vars/web/.hidden.yml", "hvar: 1\n");
        write(&d, "group_vars/web/c.yml~", "bvar: 1\n");
        write(&d, "group_vars/web/d.txt", "tvar: 1\n");
        write(&d, "group_vars/web/e.json", "{\"jvar\": 1}\n");
        let play = d.join("play.yml");
        std::fs::write(&play, "- hosts: web\n  tasks: []\n").unwrap();
        let nodes = Document::new(std::fs::read_to_string(&play).unwrap()).parse().unwrap();
        let defs = definitions(&play, &nodes);
        let seen = |n: &str| defs.iter().any(|d| d.name == n);
        for name in ["json_top", "dvar", "dvar2", "jvar"] {
            assert!(seen(name), "{name} is read by ansible but missing: {:?}", names_of(&defs));
        }
        for name in ["txt_top", "hvar", "bvar", "tvar"] {
            assert!(!seen(name), "{name} is not read by ansible but was indexed");
        }
        // Everything under `group_vars/all/` is `all`'s, however deep — the entity is the
        // directory entry, not the leaf file, and `all` has its own precedence rank.
        let _ = std::fs::remove_dir_all(&d);
        write(&d, "group_vars/all/deep/x.yml", "deep_all: 1\n");
        std::fs::write(&play, "- hosts: web\n  tasks: []\n").unwrap();
        let defs = definitions(&play, &nodes);
        let hit = defs.iter().find(|x| x.name == "deep_all").expect("nested all/ file");
        assert_eq!(hit.source, VarSource::GroupVarsAll);
    }

    /// `find_vars_files` stops at the first extension that exists, so only one of these ever
    /// loads. Measured as a chain: all four present resolves to the extension-less file, and
    /// deleting it moves the answer to `.yml` — four files, four different values.
    #[test]
    fn one_group_vars_file_per_name_wins_by_extension_order() {
        let d = std::env::temp_dir().join("ansible-lsp-t062-gvprec");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for (rel, body) in [
            ("group_vars/all", "probe: 1\n"),
            ("group_vars/all.yml", "probe: 1\n"),
            ("group_vars/all.yaml", "probe: 1\n"),
            ("group_vars/all.json", "{\"probe\": 1}\n"),
        ] {
            write(&d, rel, body);
        }
        let play = d.join("play.yml");
        std::fs::write(&play, "- hosts: all\n  tasks: []\n").unwrap();
        let nodes = Document::new(std::fs::read_to_string(&play).unwrap()).parse().unwrap();

        let winner = |defs: &[Located]| -> String {
            let hits: Vec<_> = defs.iter().filter(|x| x.name == "probe").collect();
            assert_eq!(hits.len(), 1, "one file loads, not {}: {:?}", hits.len(), hits);
            hits[0].file.file_name().unwrap().to_str().unwrap().to_string()
        };
        assert_eq!(winner(&definitions(&play, &nodes)), "all");
        std::fs::remove_file(d.join("group_vars/all")).unwrap();
        assert_eq!(winner(&definitions(&play, &nodes)), "all.yml");
        std::fs::remove_file(d.join("group_vars/all.yml")).unwrap();
        assert_eq!(winner(&definitions(&play, &nodes)), "all.yaml");
    }

    /// A directory inventory's `group_vars/` sits at the source root, not beside whichever
    /// file inside it happens to hold the hosts. Measured with `inv/sub/hosts.ini` as the
    /// only host file: `inv/group_vars/web.yml` applies, `inv/sub/group_vars/web.yml` does
    /// not. Deriving the base from each expanded file's parent got both halves wrong.
    #[test]
    fn a_directory_inventorys_group_vars_stay_at_the_source_root() {
        let d = std::env::temp_dir().join("ansible-lsp-t062-invbase");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write(&d, "ansible.cfg", "[defaults]\ninventory = inv\n");
        write(&d, "inv/sub/hosts.ini", "[web]\nnode1\n");
        write(&d, "inv/group_vars/web.yml", "at_source_root: 1\n");
        write(&d, "inv/sub/group_vars/web.yml", "beside_the_host_file: 1\n");
        let play = d.join("play.yml");
        std::fs::write(&play, "- hosts: web\n  tasks: []\n").unwrap();
        let nodes = Document::new(std::fs::read_to_string(&play).unwrap()).parse().unwrap();
        let defs = definitions(&play, &nodes);
        assert!(defs.iter().any(|x| x.name == "at_source_root"), "{:?}", names_of(&defs));
        assert!(!defs.iter().any(|x| x.name == "beside_the_host_file"));
    }

    /// The wiring half of T-178. `definitions` is what hover and go-to-definition read, and
    /// the readers being right is not the same as the index being right — the comment in
    /// `a_configured_inventory_reaches_the_index_and_a_dynamic_one_does_not` records this
    /// repo shipping that exact confusion twice.
    ///
    /// The two rows must move in opposite directions, which is why they are one test: a
    /// blanket drop passes the first assertion and fails the second, and that blanket drop
    /// is what T-178 originally prescribed.
    #[test]
    fn group_priority_reaches_the_index_from_a_host_and_never_from_a_group() {
        let d = std::env::temp_dir().join("ansible-lsp-t178-wiring");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write(&d, "ansible.cfg", "[defaults]\ninventory = hosts.ini\n");
        write(
            &d,
            "hosts.ini",
            "[web]\nnode1 ansible_group_priority=10 beside_the_host=yes\n\n             [web:vars]\nansible_group_priority=20\nbeside_the_group=yes\n",
        );
        let play = d.join("play.yml");
        std::fs::write(&play, "- hosts: web\n  tasks: []\n").unwrap();
        let nodes = Document::new(std::fs::read_to_string(&play).unwrap()).parse().unwrap();
        let defs = definitions(&play, &nodes);

        // Both controls are indexed, so an absence below is an absence and not a file that
        // was never read.
        for name in ["beside_the_host", "beside_the_group"] {
            assert!(
                defs.iter().any(|x| x.name == name),
                "{name} not indexed: {:?}",
                names_of(&defs)
            );
        }
        // Exactly one definition survives: the host one. `Group.set_variable` ate the other.
        let hits: Vec<&Located> =
            defs.iter().filter(|x| x.name == crate::inventory::GROUP_PRIORITY).collect();
        assert_eq!(
            hits.len(),
            1,
            "expected the host-line definition only, got {:?}",
            hits.iter().map(|h| h.source).collect::<Vec<_>>()
        );
        assert_eq!(hits[0].source, VarSource::Inventory);
    }

    /// T-062 end to end: an inventory named by `ansible.cfg` reaches the variable index of
    /// a playbook beside it. Asserted through `definitions` rather than the parsers, because
    /// the parsers passing proves nothing about the wiring — deleting the call site left
    /// every other test in this file green.
    #[test]
    fn a_configured_inventory_reaches_the_index_and_a_dynamic_one_does_not() {
        let d = std::env::temp_dir().join("ansible-lsp-t062-wiring");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write(&d, "ansible.cfg", "[defaults]\ninventory = hosts.ini\n");
        write(
            &d,
            "hosts.ini",
            "[web]\nnode1 ip_from_host_line=10.0.0.5\n\n[web:vars]\nfrom_group_section=yes\n",
        );
        let play = d.join("play.yml");
        std::fs::write(&play, "- hosts: web\n  tasks: []\n").unwrap();

        let nodes = Document::new(std::fs::read_to_string(&play).unwrap()).parse().unwrap();
        let defs = definitions(&play, &nodes);
        for name in ["ip_from_host_line", "from_group_section"] {
            let hit = defs
                .iter()
                .find(|x| x.name == name)
                .unwrap_or_else(|| panic!("{name} not indexed: {:?}", names_of(&defs)));
            assert_eq!(hit.source, VarSource::Inventory);
            // Host-dependent, like a named group_vars file — the hover caveat depends on it.
            assert!(hit.source.host_scoped());
            // And reachable through `hostvars`, which is the whole point of T-172.
            assert!(hit.source.visible_to_hostvars());
        }

        // A plugin config is something Ansible *runs*. We never do, so it defines nothing —
        // and, critically, indexing its own keys (`plugin`, `regions`) as variables would
        // invent names no play can use.
        write(&d, "ansible.cfg", "[defaults]\ninventory = dyn.yml\n");
        write(&d, "dyn.yml", "plugin: amazon.aws.aws_ec2\nregions:\n  - us-east-1\n");
        let defs = definitions(&play, &nodes);
        assert!(
            !defs.iter().any(|d| d.name == "plugin" || d.name == "regions"),
            "a dynamic inventory must contribute nothing: {:?}",
            names_of(&defs)
        );

        // An executable inventory is a script Ansible RUNS, and we never do. What that
        // buys is mostly forward-looking — a host list we cannot know must not be claimed
        // complete (T-062 box 8) — because a real script yields few variables anyway: a
        // bare `PORT=8080` line is an ini *host line*, whose first token is the host name.
        // The fixture below therefore carries a `[web:vars]` section, so the execute bit is
        // the only difference between indexing it and not, which is the thing under test.
        write(&d, "ansible.cfg", "[defaults]\ninventory = dyn.sh\n");
        write(&d, "dyn.sh", "#!/bin/sh\n[web:vars]\nPORT=8080\nREGION=us-east-1\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(d.join("dyn.sh"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
            let defs = definitions(&play, &nodes);
            assert!(
                !defs.iter().any(|d| d.name == "PORT" || d.name == "REGION"),
                "an executable inventory must not be harvested: {:?}",
                names_of(&defs)
            );
            // The control that makes the assertion mean something: the identical file with
            // the execute bit cleared IS read, so the silence above is the detector working
            // rather than the reader failing to find anything.
            std::fs::set_permissions(d.join("dyn.sh"), std::fs::Permissions::from_mode(0o644))
                .unwrap();
            assert!(
                definitions(&play, &nodes).iter().any(|d| d.name == "PORT"),
                "the same file, not executable, is an ordinary ini inventory"
            );
        }

        // The `group_vars/` beside the INVENTORY, which is a different directory from the
        // one beside the playbook. Both are loaded; this pair was the documented gap that
        // "needs the inventory's path, not guessed", and resolving the path closes it.
        write(&d, "ansible.cfg", "[defaults]\ninventory = inv/hosts.ini\n");
        write(&d, "inv/hosts.ini", "[web]\nnode1\n");
        write(&d, "inv/group_vars/web.yml", "beside_the_inventory: yes\n");
        write(&d, "group_vars/all.yml", "beside_the_playbook: yes\n");
        let defs = definitions(&play, &nodes);
        for name in ["beside_the_inventory", "beside_the_playbook"] {
            assert!(
                defs.iter().any(|d| d.name == name),
                "{name} missing: {:?}",
                names_of(&defs)
            );
        }

        // A DIRECTORY source, which every case above spells as a single file. `sources`
        // expands it, so each contained file is read and `read_var_dir` is reached once per
        // file — the adjacent `group_vars/` must still define its name once, not per file.
        write(&d, "ansible.cfg", "[defaults]\ninventory = dir\n");
        write(&d, "dir/a.ini", "[web:vars]\nfrom_first_file=1\n");
        write(&d, "dir/b.ini", "[db:vars]\nfrom_second_file=1\n");
        write(&d, "dir/group_vars/web.yml", "beside_the_dir: yes\n");
        let defs = definitions(&play, &nodes);
        for name in ["from_first_file", "from_second_file", "beside_the_dir"] {
            assert!(
                defs.iter().any(|d| d.name == name),
                "{name} missing from a directory inventory: {:?}",
                names_of(&defs)
            );
        }
        assert_eq!(
            defs.iter().filter(|d| d.name == "beside_the_dir").count(),
            1,
            "one definition, though the directory's two files each reach the same group_vars"
        );

        // TOML and JSON reach the index too, through the same call site. Asserted HERE
        // rather than only against `toml_vars`: the readers were correct while the wiring
        // in `read_inventory` was not, which is the shape of defect this repo has shipped
        // twice by testing a helper instead of the assembly.
        write(&d, "ansible.cfg", "[defaults]\ninventory = inv.toml\n");
        write(&d, "inv.toml", "[web.vars]\nfrom_toml = \"yes\"\n[web.hosts.node1]\ntoml_host_var = 1\n");
        let defs = definitions(&play, &nodes);
        for name in ["from_toml", "toml_host_var"] {
            assert!(
                defs.iter().any(|x| x.name == name && x.source == VarSource::Inventory),
                "{name} missing from a TOML inventory: {:?}",
                names_of(&defs)
            );
        }
        write(&d, "ansible.cfg", "[defaults]\ninventory = inv.json\n");
        write(&d, "inv.json", r#"{"web": {"vars": {"from_json": "yes"}}}"#);
        assert!(
            definitions(&play, &nodes).iter().any(|x| x.name == "from_json"),
            "a JSON inventory did not reach the index"
        );

        // A configured inventory that is not there is the normal state where inventories
        // are generated and untracked — silence, not a panic and not a message.
        write(&d, "ansible.cfg", "[defaults]\ninventory = never_generated.yml\n");
        assert!(definitions(&play, &nodes).iter().all(|d| d.source != VarSource::Inventory));
    }

    /// T-062: `ansibleLsp.inventory` stands in for `-i`, which beats the env var and the
    /// config file both (measured). Two inventories disagreeing about one variable is not a
    /// contrived fixture — it is this workspace, where `server1`'s `infiniband_ip` differs
    /// between `inventory.yml` and `inventory-lab.yml`.
    #[test]
    fn the_inventory_override_beats_ansible_cfg() {
        let d = std::env::temp_dir().join("ansible-lsp-t062-override");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write(&d, "ansible.cfg", "[defaults]\ninventory = prod.ini\n");
        write(&d, "prod.ini", "[web:vars]\ntarget_ip=10.0.0.1\n");
        write(&d, "lab.ini", "[web:vars]\ntarget_ip=192.168.0.1\n");
        let play = d.join("play.yml");
        std::fs::write(&play, "- hosts: web\n  tasks: []\n").unwrap();
        let nodes = Document::new(std::fs::read_to_string(&play).unwrap()).parse().unwrap();

        let value_with = |over: Vec<PathBuf>| -> String {
            let cache = ScanCache::default().with_inventory(over);
            let defs = definitions_with_deps_in(&play, &nodes, &cache).0;
            let hit = defs.iter().find(|x| x.name == "target_ip").expect("indexed");
            let text = std::fs::read_to_string(&hit.file).unwrap();
            hit.span.slice(&text).trim().to_string()
        };
        // Nothing set: follow the config, as a plain `ansible-playbook` would.
        assert_eq!(value_with(Vec::new()), "10.0.0.1");
        // Set: the user's `-i` wins, and the value on screen changes with it. Without this
        // the setting would be a knob that reads back but changes nothing.
        assert_eq!(value_with(vec![d.join("lab.ini")]), "192.168.0.1");

        // Several sources MERGE, the way repeated `-i` and a comma list in `ansible.cfg`
        // both do (measured: two inventories in one directory produced both hosts). Each
        // one's own names must survive, not just the last file's.
        write(&d, "extra.ini", "[web:vars]\nonly_in_extra=yes\n");
        let cache = ScanCache::default().with_inventory(vec![d.join("lab.ini"), d.join("extra.ini")]);
        let defs = definitions_with_deps_in(&play, &nodes, &cache).0;
        for name in ["target_ip", "only_in_extra"] {
            assert!(
                defs.iter().any(|x| x.name == name),
                "{name} missing when two inventories are given: {:?}",
                names_of(&defs)
            );
        }
    }

    fn names_of(defs: &[Located]) -> Vec<&str> {
        defs.iter().map(|d| d.name.as_str()).collect()
    }

    /// T-062: the two `group_vars/` shapes the old `*.yml`-only listing dropped. Both were
    /// measured — an extension-less `group_vars/all` reached the play, and with both a
    /// `webservers.yml` and a `webservers/` directory present the **directory's** value
    /// arrived. That shadowing is the opposite of role `defaults/`, so it cannot be guessed
    /// from the neighbouring rule; indexing the `.yml` here would report a value no run
    /// ever uses.
    #[test]
    fn extensionless_group_vars_load_and_a_directory_shadows_the_same_named_file() {
        let d = std::env::temp_dir().join("ansible-lsp-t062-groupvars");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write(&d, "group_vars/all", "no_extension: FROM_EXTENSIONLESS\n");
        write(&d, "group_vars/webservers.yml", "shadow_probe: FROM_YML_FILE\n");
        write(&d, "group_vars/webservers/main.yml", "shadow_probe: FROM_DIRECTORY\n");
        let play = d.join("play.yml");
        std::fs::write(&play, "- hosts: all\n  tasks: []\n").unwrap();

        let nodes = Document::new(std::fs::read_to_string(&play).unwrap()).parse().unwrap();
        let defs = definitions(&play, &nodes);
        let value_of = |name: &str| -> Option<String> {
            let d = defs.iter().find(|d| d.name == name)?;
            let text = std::fs::read_to_string(&d.file).ok()?;
            Some(d.span.slice(&text).trim().to_string())
        };
        assert_eq!(value_of("no_extension").as_deref(), Some("FROM_EXTENSIONLESS"));
        // The directory wins, and the shadowed file contributes nothing at all — not even
        // a second candidate, since a hover listing it would offer a dead value.
        assert_eq!(value_of("shadow_probe").as_deref(), Some("FROM_DIRECTORY"));
        assert_eq!(defs.iter().filter(|d| d.name == "shadow_probe").count(), 1);
    }

    #[test]
    fn effective_picks_highest_precedence_then_latest() {
        let mk = |src, start| Located {
            name: "x".into(),
            source: src,
            span: Span { start, end: start + 1 },
            file: PathBuf::from("f.yml"),
            after: None,
            condition: None,
            via: Vec::new(),
            scope: None,
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
            after: None,
            condition: None,
            via: Vec::new(),
            scope: None,
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

    /// T-206, the index consumer. `resolve_var_path` carried its own copy of the search
    /// order, so the resolver could be fixed and this half still be wrong — which is what
    /// made the tool contradict itself: go-to-definition opened one file while the index had
    /// read another.
    ///
    /// `tasks/extra.yml` is the decoy. It is a task list, so the old order — which searched
    /// the file's own dir first — resolved to it and indexed nothing at all.
    #[test]
    fn include_vars_of_a_bare_name_in_a_role_indexes_the_role_vars_file() {
        let d = std::env::temp_dir().join("ansible-lsp-t206-index");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write(&d, "roles/ad/vars/extra.yml", "other: 1\n");
        write(&d, "roles/ad/tasks/extra.yml", "- debug: {msg: decoy}\n");
        let tasks = d.join("roles/ad/tasks/main.yml");
        let src = "- include_vars: extra.yml\n";
        write(&d, "roles/ad/tasks/main.yml", src);

        let nodes = Document::new(src.to_string()).parse().unwrap();
        let defs = definitions(&tasks, &nodes);
        let other = defs
            .iter()
            .find(|x| x.name == "other" && x.source == VarSource::IncludeVars)
            .expect("indexed from the role's vars file, not the task-dir decoy");
        assert_eq!(other.file, d.join("roles/ad/vars/extra.yml"));
    }

    /// T-207: a name loaded at two precedence levels must be indexed at both.
    ///
    /// A role's own `vars/main.yml` re-included by its `tasks/main.yml` is loaded twice —
    /// role vars at 15 and `include_vars` at 18 — and 18 is the level in effect against a
    /// task-level `vars:` at 17. `dedup` keys on `(name, file, span)` and ignores `source`,
    /// so the second is dropped and the index keeps the level that is **not** in effect.
    ///
    /// Fixed by putting `source` in [`dedup`]'s key.
    #[test]
    fn a_reinclude_of_the_roles_own_vars_is_indexed_at_both_precedence_levels() {
        let d = std::env::temp_dir().join("ansible-lsp-t207");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write(&d, "roles/ad/vars/main.yml", "thing: FROM_ROLE_VARS\n");
        let tasks = d.join("roles/ad/tasks/main.yml");
        let src = "- include_vars: main.yml\n";
        write(&d, "roles/ad/tasks/main.yml", src);

        let nodes = Document::new(src.to_string()).parse().unwrap();
        let defs = definitions(&tasks, &nodes);
        let sources: Vec<_> = defs.iter().filter(|x| x.name == "thing").map(|x| x.source).collect();
        assert!(sources.contains(&VarSource::RoleVars), "auto-loaded: {sources:?}");
        assert!(
            sources.contains(&VarSource::IncludeVars),
            "the include is the level in effect, and it is missing: {sources:?}"
        );
        // The one that answers a lookup is the higher of the two.
        assert_eq!(
            effective(&defs.iter().filter(|x| x.name == "thing").cloned().collect::<Vec<_>>())
                .map(|d| d.source),
            Some(VarSource::IncludeVars)
        );
    }

    /// T-208: `include_vars` loads at a point in the run, so a use above it cannot see what
    /// it defines and a use below it can. The pair is the claim — either half alone passes
    /// under a rule that always answers the same way.
    #[test]
    fn an_include_vars_definition_reaches_uses_below_it_and_not_above() {
        let d = std::env::temp_dir().join("ansible-lsp-t208-order");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write(&d, "vars/late.yml", "late_key: 1\n");
        let play = d.join("play.yml");
        let src = "- hosts: all\n  tasks:\n    - debug: {msg: above}\n    - include_vars: vars/late.yml\n    - debug: {msg: below}\n";
        write(&d, "play.yml", src);

        let nodes = Document::new(src.to_string()).parse().unwrap();
        let defs = definitions(&play, &nodes);
        let def = defs.iter().find(|x| x.name == "late_key").expect("indexed");
        let above = src.find("msg: above").unwrap();
        let below = src.find("msg: below").unwrap();
        assert!(!def.in_effect_at(&play, above), "the include has not run at the earlier use");
        assert!(def.in_effect_at(&play, below), "and has at the later one");
    }

    /// T-208: each `include_vars` is ordered against **its own** task, not against the first
    /// one in the file — including one nested in a `block:`. A single shared site would pass
    /// the two-task test above, so this is the one that catches it.
    #[test]
    fn each_include_vars_is_ordered_against_its_own_task() {
        let d = std::env::temp_dir().join("ansible-lsp-t208-multi");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write(&d, "vars/a.yml", "k_a: 1\n");
        write(&d, "vars/b.yml", "k_b: 1\n");
        write(&d, "vars/c.yml", "k_c: 1\n");
        let play = d.join("play.yml");
        let src = concat!(
            "- hosts: all\n",
            "  tasks:\n",
            "    - include_vars: vars/a.yml\n",
            "    - debug: {msg: MIDDLE}\n",
            "    - include_vars: vars/b.yml\n",
            "    - block:\n",
            "        - include_vars: vars/c.yml\n",
        );
        write(&d, "play.yml", src);

        let nodes = Document::new(src.to_string()).parse().unwrap();
        let defs = definitions(&play, &nodes);
        let mid = src.find("MIDDLE").unwrap();
        let at = |n: &str| {
            let def = defs.iter().find(|x| x.name == n).unwrap_or_else(|| panic!("{n} missing"));
            def.in_effect_at(&play, mid)
        };
        assert!(at("k_a"), "loaded before this point");
        assert!(!at("k_b"), "loaded after it");
        assert!(!at("k_c"), "and so is the one inside the block");

        // Three distinct sites, not one shared: the sites must be strictly increasing, which
        // is the property a single stamp would break while still passing the checks above.
        let sites: Vec<usize> = ["k_a", "k_b", "k_c"]
            .iter()
            .map(|n| defs.iter().find(|x| x.name == *n).unwrap().after.as_ref().unwrap().1)
            .collect();
        assert!(sites[0] < sites[1] && sites[1] < sites[2], "one site per task: {sites:?}");
    }

    /// T-208 for the **dir** form. Every other ordering test uses the file form, and the two
    /// take different branches — the dir form runs the ported plugin walk and reads a list of
    /// files, so a fix applied only to the file branch would leave this one always-in-effect.
    #[test]
    fn the_dir_form_is_ordered_against_its_task_too() {
        let d = std::env::temp_dir().join("ansible-lsp-t208-dir");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write(&d, "conf/x.yml", "dir_key: 1\n");
        let play = d.join("play.yml");
        let src = concat!(
            "- hosts: all\n",
            "  tasks:\n",
            "    - debug: {msg: ABOVE}\n",
            "    - include_vars: {dir: conf}\n",
            "    - debug: {msg: BELOW}\n",
        );
        write(&d, "play.yml", src);

        let nodes = Document::new(src.to_string()).parse().unwrap();
        let defs = definitions(&play, &nodes);
        let def = defs.iter().find(|x| x.name == "dir_key").expect("dir form is indexed");
        assert!(!def.in_effect_at(&play, src.find("ABOVE").unwrap()), "not before the load");
        assert!(def.in_effect_at(&play, src.find("BELOW").unwrap()), "and yes after it");
    }

    /// T-208's other control: ordering is for "what does this read *here*", never for "is
    /// this name ever set". `undefined_uses` filters on `reaches`, which is scope-only — a
    /// name loaded by a later `include_vars` is defined, just not yet, and reporting it
    /// undefined would be the false positive rule 3's corollary exists to prevent.
    #[test]
    fn a_name_loaded_by_a_later_include_vars_is_not_undefined() {
        let d = std::env::temp_dir().join("ansible-lsp-t208-undef");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write(&d, "vars/late.yml", "late_key: 1\n");
        let play = d.join("play.yml");
        let src = "- hosts: all\n  tasks:\n    - debug: {msg: \"{{ late_key }}\"}\n    - include_vars: vars/late.yml\n";
        write(&d, "play.yml", src);

        let nodes = Document::new(src.to_string()).parse().unwrap();
        let flagged: Vec<String> =
            undefined_uses(&play, &nodes, src).into_iter().map(|u| u.name).collect();
        assert!(!flagged.contains(&"late_key".to_string()), "not undefined, just later: {flagged:?}");
    }

    /// T-208's conservative arm: an include in another file cannot be ordered against this
    /// use, so the definition is kept rather than guessed away. Dropping it would invent
    /// "undefined" for every variable a parent play loads before including this file.
    #[test]
    fn an_include_vars_definition_from_another_file_is_kept() {
        let d = std::env::temp_dir().join("ansible-lsp-t208-crossfile");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write(&d, "vars/late.yml", "late_key: 1\n");
        write(&d, "inner.yml", "- debug: {msg: \"{{ late_key }}\"}\n");
        let play = d.join("play.yml");
        let src = "- hosts: all\n  tasks:\n    - include_vars: vars/late.yml\n    - include_tasks: inner.yml\n";
        write(&d, "play.yml", src);

        let nodes = Document::new(src.to_string()).parse().unwrap();
        let def = definitions(&play, &nodes)
            .into_iter()
            .find(|x| x.name == "late_key")
            .expect("indexed");
        // Offset 0 of a different file: no ordering is possible, so it must still apply.
        assert!(def.in_effect_at(&d.join("inner.yml"), 0), "cross-file stays conservative");
    }

    /// The control this fix must not break: sources that bind before any task runs apply
    /// everywhere in the file, including at offset 0. If ordering leaked to these, every
    /// play var would stop applying to the tasks above its own `vars:` block.
    #[test]
    fn sources_that_bind_before_the_run_are_never_ordered() {
        let d = std::env::temp_dir().join("ansible-lsp-t208-unordered");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write(&d, "vars/early.yml", "from_file: 1\n");
        let play = d.join("play.yml");
        let src = "- hosts: all\n  vars_files: [vars/early.yml]\n  vars:\n    play_var: 2\n  tasks:\n    - debug: {msg: t}\n";
        write(&d, "play.yml", src);

        let nodes = Document::new(src.to_string()).parse().unwrap();
        let defs = definitions(&play, &nodes);
        for name in ["from_file", "play_var"] {
            let def = defs.iter().find(|x| x.name == name).unwrap_or_else(|| panic!("{name}"));
            assert!(def.in_effect_at(&play, 0), "{name} ({:?}) binds before the run", def.source);
        }
    }

    /// T-207's second pair, measured on 2.21.3 the same way: a `vars_files:` entry re-read by
    /// `include_vars:` is loaded at 14 and at 18, and 18 beats a task-level `vars:` at 17.
    /// Nothing about the collapse was specific to roles — it was any one file reaching the
    /// index by two routes at two levels.
    #[test]
    fn a_vars_files_entry_re_read_by_include_vars_is_indexed_at_both_levels() {
        let d = std::env::temp_dir().join("ansible-lsp-t207-varsfiles");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write(&d, "vars/shared.yml", "thing: FROM_FILE\n");
        let play = d.join("play.yml");
        let src = "- hosts: all\n  vars_files: [vars/shared.yml]\n  tasks:\n    - include_vars: vars/shared.yml\n";
        write(&d, "play.yml", src);

        let nodes = Document::new(src.to_string()).parse().unwrap();
        let defs = definitions(&play, &nodes);
        let sources: Vec<_> = defs.iter().filter(|x| x.name == "thing").map(|x| x.source).collect();
        assert!(sources.contains(&VarSource::VarsFiles), "why the file is in scope: {sources:?}");
        assert!(sources.contains(&VarSource::IncludeVars), "the level in effect: {sources:?}");
    }

    /// The behaviour [`dedup`] exists for, and the control for the change above: two routes to
    /// the *same* load carry the same `source`, so they still collapse to one entry. Without
    /// this, putting `source` in the key would read as "keep everything".
    #[test]
    fn the_same_file_included_twice_is_still_one_definition() {
        let d = std::env::temp_dir().join("ansible-lsp-t207-twice");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write(&d, "vars/shared.yml", "thing: 1\n");
        let play = d.join("play.yml");
        let src = "- hosts: all\n  tasks:\n    - include_vars: vars/shared.yml\n    - include_vars: vars/shared.yml\n";
        write(&d, "play.yml", src);

        let nodes = Document::new(src.to_string()).parse().unwrap();
        let defs = definitions(&play, &nodes);
        let sources: Vec<_> = defs.iter().filter(|x| x.name == "thing").map(|x| x.source).collect();
        assert_eq!(sources, vec![VarSource::IncludeVars], "one load, one entry");
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

    #[test]
    fn vars_files_group_indexes_only_the_winner() {
        let d = std::env::temp_dir().join("ansible-lsp-t016-vf-group");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        // Both alternatives exist — Ansible loads only the first, so only it is indexed.
        write(&d, "vars/site-local.yml", "from_local: 1\n");
        write(&d, "vars/shared.yml", "from_shared: 2\n");
        let play = d.join("play.yml");
        std::fs::write(
            &play,
            "- hosts: all\n  vars_files:\n    - - vars/site-local.yml\n      - vars/shared.yml\n",
        )
        .unwrap();

        let nodes = Document::new(std::fs::read_to_string(&play).unwrap()).parse().unwrap();
        let defs = definitions(&play, &nodes);
        assert!(defs.iter().any(|x| x.name == "from_local"));
        assert!(
            !defs.iter().any(|x| x.name == "from_shared"),
            "the shadowed alternative must not be indexed"
        );
    }

    #[test]
    fn vars_files_indexing_prefers_the_vars_subdir() {
        let d = std::env::temp_dir().join("ansible-lsp-t016-vf-order");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        // Same name in the play dir and its vars/ — Ansible loads the vars/ copy.
        write(&d, "x.yml", "which: plain\n");
        write(&d, "vars/x.yml", "which: vars_subdir\n");
        let play = d.join("play.yml");
        std::fs::write(&play, "- hosts: all\n  vars_files: [x.yml]\n").unwrap();

        let nodes = Document::new(std::fs::read_to_string(&play).unwrap()).parse().unwrap();
        let defs = definitions(&play, &nodes);
        let def = defs.iter().find(|x| x.name == "which").unwrap();
        assert!(def.file.ends_with("vars/x.yml"), "got {:?}", def.file);
    }

    #[test]
    fn vars_files_templated_alternative_abandons_the_entry() {
        let d = std::env::temp_dir().join("ansible-lsp-t016-vf-tmpl");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write(&d, "vars/fallback.yml", "from_fallback: 1\n");
        let play = d.join("play.yml");
        // Were the templated first alternative to resolve at runtime it would win, so
        // crediting fallback.yml here could attribute variables to the wrong file.
        std::fs::write(
            &play,
            "- hosts: all\n  vars_files:\n    - - \"vars/{{ env }}.yml\"\n      - vars/fallback.yml\n",
        )
        .unwrap();

        let nodes = Document::new(std::fs::read_to_string(&play).unwrap()).parse().unwrap();
        let defs = definitions(&play, &nodes);
        assert!(!defs.iter().any(|x| x.name == "from_fallback"));
    }
}

#[cfg(test)]
mod perf {
    use super::*;
    use crate::parse::Document;
    use crate::workspace::yaml_files;
    use std::time::Instant;

    /// The T-076 A/B, on whatever tree you point it at: walk every file with a cache per
    /// file (what the scan did before) and then with one cache for the pass. Prints both
    /// times and the edges-to-files ratio — the platform-independent half, since on a fast
    /// machine the milliseconds can't see this phase at all.
    ///
    /// `cargo test --release var_walk -- --ignored --nocapture [dir]`, dir via `T076_ROOT`.
    #[test]
    #[ignore = "profiling aid: cargo test --release var_walk -- --ignored --nocapture"]
    fn var_walk_shared_vs_per_file() {
        // Tests run from the crate dir, so the default is the repo's own demo tree.
        let root = match std::env::var("T076_ROOT") {
            Ok(r) => PathBuf::from(r),
            Err(_) => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../demo"),
        };
        if !root.is_dir() {
            println!("no such tree: {}", root.display());
            return;
        }
        let files: Vec<(PathBuf, Vec<Node>)> = yaml_files(&root)
            .into_iter()
            .filter_map(|p| {
                let text = std::fs::read_to_string(&p).ok()?;
                let nodes = Document::new(text).parse()?;
                Some((p, nodes))
            })
            .collect();

        let run = |cache: Option<&ScanCache>| {
            let t = Instant::now();
            let mut defs = 0;
            for (p, nodes) in &files {
                defs += match cache {
                    Some(c) => definitions_with_deps_in(p, nodes, c).0.len(),
                    None => definitions_with_deps(p, nodes).0.len(),
                };
            }
            (t.elapsed(), defs)
        };

        // Warm the OS page cache first. Without this the *first* run pays every cold miss
        // and the second reads a warmed tree — on a 9p mount that is the difference between
        // 1.4 ms and 0.5 ms per stat, so whichever pass ran second looked ~2x better than it
        // was. Both passes measure warm now; for cold numbers, read the editor's scan line.
        let _ = run(Some(&ScanCache::default()));

        let (per_file, defs_a) = run(None);
        // Counting *under* the memo: what actually reached the filesystem (T-085).
        let disk = std::sync::Arc::new(crate::fs::Counting::new(crate::fs::StdFs));
        let shared = ScanCache::new(disk.clone());
        let (pass, defs_b) = run(Some(&shared));
        let s = shared.stats();
        println!("{} files under {}", files.len(), root.display());
        println!("  cache per file: {per_file:?}");
        println!("  one per pass:   {pass:?}");
        println!(
            "  var-walk: {} edges -> {} files ({} uncached), {} reads, {} contexts, \
             {} ansible.cfg",
            s.edges, s.files, s.uncached, s.reads, s.contexts, s.configs
        );
        let fs = disk.stats();
        println!(
            "  syscalls: {} in {:.0} ms ({} missing){}",
            fs.calls(),
            fs.nanos() as f64 / 1e6,
            fs.misses(),
            match fs.distinct() {
                Some(d) => format!(", {d} distinct paths"),
                None => String::new(),
            }
        );
        for (name, c) in fs.each() {
            let (calls, misses, nanos) = (
                c.calls.load(std::sync::atomic::Ordering::Relaxed),
                c.misses.load(std::sync::atomic::Ordering::Relaxed),
                c.nanos.load(std::sync::atomic::Ordering::Relaxed),
            );
            if calls > 0 {
                println!(
                    "    {name:<10} {calls:>6} calls  {:>7.1} ms  {misses} missing",
                    nanos as f64 / 1e6
                );
            }
        }
        for (p, n) in fs.top_paths(6) {
            println!("    repeat x{n:<4} {}", p.display());
        }
        // The point of the whole ticket: sharing must not change a single definition.
        assert_eq!(defs_a, defs_b, "shared cache changed the result");
    }
}

#[cfg(test)]
mod parallel_spike {
    use super::*;
    use crate::parse::Document;
    use crate::workspace::yaml_files;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    /// How much does the walk gain from running files concurrently? (T-085 follow-up)
    ///
    /// The walk is ~97% filesystem *latency* — 0.5–1.4 ms per round trip on a 9p mount —
    /// so nothing is CPU-bound and overlapping requests should scale until the server
    /// saturates. That ceiling is the thing worth knowing before rewriting the scan loop,
    /// because `ScanCache`'s single lock and the LSP's publish bookkeeping both get harder
    /// under concurrency and are only worth paying for if the ceiling is high.
    ///
    /// `cargo test --release parallel_spike -- --ignored --nocapture`
    #[test]
    #[ignore = "profiling aid: cargo test --release parallel_spike -- --ignored --nocapture"]
    fn how_far_does_concurrency_get_us() {
        let root = match std::env::var("T076_ROOT") {
            Ok(r) => PathBuf::from(r),
            Err(_) => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../demo"),
        };
        if !root.is_dir() {
            println!("no such tree: {}", root.display());
            return;
        }
        let files: Vec<(PathBuf, Vec<Node>)> = yaml_files(&root)
            .into_iter()
            .filter_map(|p| {
                let text = std::fs::read_to_string(&p).ok()?;
                Some((p, Document::new(text).parse()?))
            })
            .collect();

        // Warm the page cache so every row below measures the same thing.
        let _ = {
            let c = ScanCache::default();
            files.iter().map(|(p, n)| definitions_with_deps_in(p, n, &c).0.len()).sum::<usize>()
        };

        println!("{} files under {}", files.len(), root.display());
        let mut base = 0f64;
        for threads in [1usize, 2, 4, 8, 16, 32] {
            let cache = ScanCache::default();
            let next = AtomicUsize::new(0);
            let defs = AtomicUsize::new(0);
            let t = Instant::now();
            std::thread::scope(|s| {
                for _ in 0..threads {
                    s.spawn(|| loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some((p, nodes)) = files.get(i) else { return };
                        let n = definitions_with_deps_in(p, nodes, &cache).0.len();
                        defs.fetch_add(n, Ordering::Relaxed);
                    });
                }
            });
            let ms = t.elapsed().as_secs_f64() * 1e3;
            if threads == 1 {
                base = ms;
            }
            let s = cache.stats();
            println!(
                "  {threads:>2} threads  {ms:>8.0} ms  {:>5.2}x   ({} defs, {} files walked, {} uncached)",
                base / ms,
                defs.load(Ordering::Relaxed),
                s.files,
                s.uncached
            );
        }
    }
}
