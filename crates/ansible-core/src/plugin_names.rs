//! T-109, second half: `strategy:`, `connection:` and `become_method:` must name a plugin.
//!
//! Measured on ansible-core 2.21.3: all three pass `--syntax-check`, and an unknown name fails
//! at run time — `Invalid play strategy specified: …` when the play starts, `the connection
//! plugin '…' was not found` on the task, and for `become_method` `Invalid become method
//! specified, could not find matching plugin: '…'` — the last only once `become` is in effect.
//! Without `become` the value is inert (measured), but `become` can arrive from cfg, `-b` or
//! inventory, so the name is checked regardless and the message says when it bites.
//!
//! Where a name is found decides which spellings reach it (measured with a local plugin):
//!
//! | found in                                    | bare | `ansible.legacy.x` | `ansible.builtin.x` |
//! | ------------------------------------------- | ---- | ------------------ | ------------------- |
//! | the package, or core's routing table         | yes  | yes                | yes                 |
//! | a `*_plugins` folder or a configured path   | yes  | yes                | **no**              |
//!
//! Any other collection's plugin is not judged: the collection may be installed where the play
//! runs and not here, and a collection's own routing can add names. No install → no answer.

use crate::ast::{Ast, Directive, PlayItem, Stmt};
use crate::fs::{Fs, Kind};
use crate::parse::{node_with_span, Node, Span};
use std::path::{Path, PathBuf};

/// Rule id, for `# noqa: unknown-plugin` and for display.
pub const RULE_ID: &str = "unknown-plugin";

/// The three plugin types a keyword value has to name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PluginKind {
    Strategy,
    Connection,
    Become,
}

impl PluginKind {
    /// The plugin type's name — its package dir, routing section and `<type>_plugins` stem.
    pub fn type_name(self) -> &'static str {
        match self {
            PluginKind::Strategy => "strategy",
            PluginKind::Connection => "connection",
            PluginKind::Become => "become",
        }
    }

    fn keyword(self) -> &'static str {
        match self {
            PluginKind::Strategy => "strategy",
            PluginKind::Connection => "connection",
            PluginKind::Become => "become_method",
        }
    }
}

/// Where a name was found, as far as the rule needs to know. `Local` is a plugin folder —
/// reachable bare or as `ansible.legacy.*`, never as `ansible.builtin.*` (measured).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Found {
    /// Shipped in the ansible-core package, or listed in its routing table.
    Builtin,
    /// In a `*_plugins` folder or a configured plugin path.
    Local,
    Nowhere,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Fails every run: `ansible.builtin.<name>` the package does not have.
    Error,
    /// Not found in the folders we can see, which may not be all of them.
    Warning,
}

#[derive(Debug, Clone)]
pub struct Problem {
    /// The keyword's value.
    pub span: Span,
    pub tier: Tier,
    pub message: String,
    pub rule: &'static str,
}

/// `lookup(kind, bare_name)` answers where a plugin is, or `None` when it cannot tell (no
/// Ansible install detected), which silences the rule.
pub fn problems(
    ast: &Ast,
    nodes: &[Node],
    lookup: &dyn Fn(PluginKind, &str) -> Option<Found>,
) -> Vec<Problem> {
    let mut out = Vec::new();
    let mut check = |ds: &[Directive], kinds: &[PluginKind]| {
        for kind in kinds {
            // The last occurrence is the one Ansible loads.
            let Some(d) = ds.iter().rev().find(|d| d.key == kind.keyword()) else { continue };
            let Some(value) = node_with_span(nodes, d.value).and_then(Node::as_str).map(str::trim) else { continue };
            if let Some((tier, message)) = judge(*kind, value, lookup) {
                out.push(Problem { span: d.value, tier, message, rule: RULE_ID });
            }
        }
    };
    const PLAY: &[PluginKind] = &[PluginKind::Strategy, PluginKind::Connection, PluginKind::Become];
    const TASK: &[PluginKind] = &[PluginKind::Connection, PluginKind::Become];
    fn stmts<'a>(s: &'a Stmt, out: &mut Vec<(&'a [Directive], bool)>) {
        match s {
            Stmt::Block(b) => {
                out.push((&b.directives, false));
                b.block.iter().chain(&b.rescue).chain(&b.always).for_each(|c| stmts(c, out));
            }
            Stmt::Task(t) => out.push((&t.directives, false)),
        }
    }
    let mut sites: Vec<(&[Directive], bool)> = Vec::new();
    match ast {
        Ast::Playbook(items) => {
            for item in items {
                let PlayItem::Play(p) = item else { continue };
                sites.push((&p.directives, true));
                for s in p.pre_tasks.iter().chain(&p.tasks).chain(&p.post_tasks).chain(&p.handlers) {
                    stmts(s, &mut sites);
                }
            }
        }
        Ast::Tasks(list) => list.iter().for_each(|s| stmts(s, &mut sites)),
        Ast::Other => {}
    }
    for (ds, is_play) in sites {
        check(ds, if is_play { PLAY } else { TASK });
    }
    out
}

/// The message for a value no plugin answers to, or `None` when it is fine or not judged.
fn judge(kind: PluginKind, value: &str, lookup: &dyn Fn(PluginKind, &str) -> Option<Found>) -> Option<(Tier, String)> {
    if value.is_empty() || value.contains("{{") || value.contains("{%") {
        return None;
    }
    let (name, builtin_only) = if let Some(n) = value.strip_prefix("ansible.builtin.") {
        (n, true)
    } else if let Some(n) = value.strip_prefix("ansible.legacy.") {
        (n, false)
    } else if value.contains('.') {
        return None; // another collection's plugin
    } else {
        (value, false)
    };
    let found = lookup(kind, name)?;
    let local_as_builtin = builtin_only && found == Found::Local;
    if found == Found::Builtin || (found == Found::Local && !builtin_only) {
        return None;
    }
    let fails = match kind {
        PluginKind::Strategy => format!("the play fails at start with \"Invalid play strategy specified: {value}\""),
        PluginKind::Connection => format!("the task fails with \"the connection plugin '{value}' was not found\""),
        PluginKind::Become => format!(
            "once become is in effect the task fails with \"Invalid become method specified, could not \
             find matching plugin: '{value}'\""
        ),
    };
    let t = kind.type_name();
    // `ansible.builtin` is the package folder alone (`loader.py:807`), a closed set: missing
    // there fails every run. `become_method` only fails once become is on, which can be set
    // anywhere, so it stays a warning like any name looked up in folders we may not all see.
    let tier = if builtin_only && kind != PluginKind::Become { Tier::Error } else { Tier::Warning };
    Some((tier, if local_as_builtin {
        format!(
            "`{name}` is a local {t} plugin, which is `ansible.legacy`, never `ansible.builtin` — \
             {fails}. Write `{name}` or `ansible.legacy.{name}`."
        )
    } else {
        format!(
            "no {t} plugin named `{name}` — {fails}, though `--syntax-check` passes. Searched: \
             the Ansible install, `{t}_plugins/` folders, and the `{t}_plugins` path from ansible.cfg \
             or `ANSIBLE_{}_PLUGINS`.",
            t.to_uppercase()
        )
    }))
}

/// Where plugin `name` of `kind` is, for a real install: the package's `plugins/<type>/`, then
/// core's routing table, then `dirs` (from [`crate::workspace::FileContext::plugin_dirs`]), then
/// any `<type>_plugins/` folder anywhere under `root` — a role's folder reaches every play in
/// the run, and which roles run is not known here. The walk only happens for a name every
/// cheaper place missed, which is the rare case.
pub fn find(
    kind: PluginKind,
    name: &str,
    package_dir: &Path,
    routing: &crate::install::RoutingTable,
    dirs: &[PathBuf],
    root: Option<&Path>,
    fs: &dyn Fs,
) -> Found {
    let t = kind.type_name();
    let file = format!("{name}.py");
    if fs.is_file(&package_dir.join("plugins").join(t).join(&file)) || routing.lists(t, name) {
        return Found::Builtin;
    }
    if dirs.iter().any(|d| fs.is_file(&d.join(&file))) {
        return Found::Local;
    }
    if root.is_some_and(|r| anywhere_under(r, &format!("{t}_plugins"), &file, fs)) {
        return Found::Local;
    }
    Found::Nowhere
}

/// A `<sub>/<file>` somewhere under `root`, skipping hidden and build directories.
fn anywhere_under(root: &Path, sub: &str, file: &str, fs: &dyn Fs) -> bool {
    let mut stack = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        for (p, kind) in fs.read_dir(&dir) {
            if kind != Kind::Dir {
                continue;
            }
            let Some(n) = p.file_name().map(|n| n.to_string_lossy().to_string()) else { continue };
            if n.starts_with('.') || matches!(n.as_str(), "node_modules" | "target" | "__pycache__") {
                continue;
            }
            if n == sub && fs.is_file(&p.join(file)) {
                return true;
            }
            if depth < 12 {
                stack.push((p, depth + 1));
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::Document;

    /// A fake install: builtins `linear free ssh local sudo`, routed `podman doas`, and a local
    /// folder holding `demo_steps demo_pipe`.
    fn lookup(_: PluginKind, name: &str) -> Option<Found> {
        Some(match name {
            "linear" | "free" | "ssh" | "local" | "sudo" | "podman" | "doas" => Found::Builtin,
            "demo_steps" | "demo_pipe" => Found::Local,
            _ => Found::Nowhere,
        })
    }

    fn flagged(src: &str) -> Vec<String> {
        let nodes = Document::new(src.into()).parse().unwrap();
        problems(&crate::ast::build(&nodes), &nodes, &lookup)
            .into_iter()
            .map(|p| p.span.slice(src).to_string())
            .collect()
    }

    fn messages(src: &str) -> Vec<String> {
        let nodes = Document::new(src.into()).parse().unwrap();
        problems(&crate::ast::build(&nodes), &nodes, &lookup).into_iter().map(|p| p.message).collect()
    }

    fn play(kw: &str) -> String {
        format!("- hosts: all\n  {kw}\n  tasks: []\n")
    }

    fn tiers(src: &str) -> Vec<Tier> {
        let nodes = Document::new(src.into()).parse().unwrap();
        problems(&crate::ast::build(&nodes), &nodes, &lookup).into_iter().map(|p| p.tier).collect()
    }

    /// `ansible.builtin` is the package's folder and nothing else, so a name missing from it
    /// fails every run — measured for strategy and connection, local plugin or none at all.
    #[test]
    fn a_builtin_spelling_the_package_lacks_is_an_error() {
        for kw in [
            "strategy: ansible.builtin.random",
            "strategy: ansible.builtin.demo_steps",
            "connection: ansible.builtin.nothere",
            "connection: ansible.builtin.demo_pipe",
        ] {
            assert_eq!(tiers(&play(kw)), vec![Tier::Error], "{kw}");
        }
    }

    /// A bare name is looked up in folders we may not see all of (an env path the editor never
    /// got), so not finding it is a warning.
    #[test]
    fn a_bare_or_legacy_name_found_nowhere_is_a_warning() {
        assert_eq!(tiers(&play("strategy: random")), vec![Tier::Warning]);
        assert_eq!(tiers(&play("connection: ansible.legacy.locl")), vec![Tier::Warning]);
    }

    /// Measured: `become_method: ansible.builtin.nothere` runs clean until become is on, and
    /// become can come from anywhere — inventory, a parent play, the command line.
    #[test]
    fn a_builtin_become_method_the_package_lacks_stays_a_warning() {
        assert_eq!(tiers(&play("become_method: ansible.builtin.sude")), vec![Tier::Warning]);
    }

    #[test]
    fn an_unknown_strategy_fires() {
        assert_eq!(flagged(&play("strategy: random")), vec!["random"]);
    }

    #[test]
    fn an_unknown_connection_fires_on_a_play_a_block_and_a_task() {
        assert_eq!(flagged(&play("connection: locl")), vec!["locl"]);
        let src = "- hosts: all\n  tasks:\n    - block:\n        - command: x\n          connection: loc2\n      connection: loc1\n";
        assert_eq!(flagged(src), vec!["loc1", "loc2"]);
    }

    #[test]
    fn an_unknown_become_method_fires_with_or_without_become() {
        assert_eq!(flagged(&play("become_method: sude")), vec!["sude"]);
        assert_eq!(flagged("- hosts: all\n  become: true\n  become_method: sude\n  tasks: []\n"), vec!["sude"]);
    }

    /// A role's task file is checked too.
    #[test]
    fn a_task_file_is_checked() {
        assert_eq!(flagged("- command: x\n  connection: locl\n"), vec!["locl"]);
    }

    #[test]
    fn known_names_are_silent() {
        for kw in ["strategy: linear", "connection: ssh", "become_method: sudo", "connection: podman", "become_method: doas"] {
            assert_eq!(flagged(&play(kw)), Vec::<String>::new(), "{kw}");
        }
    }

    /// Measured: a local folder answers to the bare name and to `ansible.legacy.*`.
    #[test]
    fn local_plugins_are_silent_bare_and_as_legacy() {
        for kw in ["strategy: demo_steps", "strategy: ansible.legacy.demo_steps", "connection: ansible.legacy.demo_pipe"] {
            assert_eq!(flagged(&play(kw)), Vec::<String>::new(), "{kw}");
        }
    }

    /// Measured: `ansible.builtin.demo_steps` fails although `demo_steps` runs.
    #[test]
    fn a_local_plugin_spelled_as_builtin_fires_and_says_why() {
        assert_eq!(flagged(&play("strategy: ansible.builtin.demo_steps")), vec!["ansible.builtin.demo_steps"]);
        let m = &messages(&play("connection: ansible.builtin.demo_pipe"))[0];
        assert!(m.contains("ansible.legacy"), "{m}");
    }

    #[test]
    fn builtin_spellings_of_builtins_are_silent() {
        for kw in ["strategy: ansible.builtin.free", "connection: ansible.builtin.local", "become_method: ansible.legacy.sudo"] {
            assert_eq!(flagged(&play(kw)), Vec::<String>::new(), "{kw}");
        }
    }

    /// Other collections are not judged: a collection may be installed where the play runs.
    #[test]
    fn other_collections_are_silent() {
        assert_eq!(flagged(&play("strategy: nosuch.coll.thing")), Vec::<String>::new());
        assert_eq!(flagged(&play("connection: community.docker.nosuchconn")), Vec::<String>::new());
    }

    #[test]
    fn templated_values_are_silent() {
        assert_eq!(flagged(&play("strategy: \"{{ s }}\"")), Vec::<String>::new());
    }

    #[test]
    fn no_install_means_no_answer() {
        let src = play("strategy: random");
        let nodes = Document::new(src.clone()).parse().unwrap();
        assert!(problems(&crate::ast::build(&nodes), &nodes, &|_, _| None).is_empty());
    }

    /// `strategy` is a play keyword; on a task it is an invalid attribute, reported elsewhere.
    #[test]
    fn strategy_off_a_play_is_not_judged() {
        assert_eq!(flagged("- command: x\n  strategy: random\n"), Vec::<String>::new());
    }

    #[test]
    fn messages_quote_ansibles_own_failure() {
        assert!(messages(&play("strategy: random"))[0].contains("Invalid play strategy specified"));
        assert!(messages(&play("connection: locl"))[0].contains("was not found"));
        let m = &messages(&play("become_method: sude"))[0];
        assert!(m.contains("could not find matching plugin") && m.contains("once become is in effect"), "{m}");
    }

    /// `find` against a real tree: package file, routing entry, a configured dir, a role's
    /// folder found by the walk, and a hidden folder the walk must skip.
    #[test]
    fn find_searches_package_routing_dirs_then_the_whole_tree() {
        let base = std::env::temp_dir().join(format!("t109_find_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let mk = |rel: &str| {
            let f = base.join(rel);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(&f, "").unwrap();
        };
        mk("pkg/plugins/strategy/linear.py");
        mk("cfgdir/mine.py");
        mk("proj/roles/r/strategy_plugins/deep.py");
        mk("proj/.hidden/strategy_plugins/secret.py");
        let routing = crate::install::RoutingTable::parse("plugin_routing:\n  strategy:\n    moved:\n      redirect: x.y.moved\n");
        let pkg = base.join("pkg");
        let dirs = vec![base.join("cfgdir")];
        let root = base.join("proj");
        let f = |n: &str| find(PluginKind::Strategy, n, &pkg, &routing, &dirs, Some(&root), &crate::fs::StdFs);
        assert_eq!(f("linear"), Found::Builtin);
        assert_eq!(f("moved"), Found::Builtin);
        assert_eq!(f("mine"), Found::Local);
        assert_eq!(f("deep"), Found::Local, "a role's folder anywhere in the tree");
        assert_eq!(f("secret"), Found::Nowhere, "hidden dirs are not walked");
        assert_eq!(f("random"), Found::Nowhere);
        assert_eq!(
            find(PluginKind::Connection, "linear", &pkg, &routing, &dirs, Some(&root), &crate::fs::StdFs),
            Found::Nowhere,
            "per type"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
