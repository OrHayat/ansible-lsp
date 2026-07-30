//! Which variables a playbook mutates while it runs.
//!
//! Exists for one rule. A `when:` on `import_playbook` is copied onto every task in the
//! imported playbook and evaluated per task — so if the imported playbook *sets* the
//! variable the condition reads, the condition flips partway through and the playbook
//! half-executes. Everything before the `set_fact` runs, everything after silently
//! skips, and `set_fact` is host-scoped so a cluster can split.
//!
//! Found in the real repo: `playbooks/daos-deploy-full.yml` imports
//! `daos-storage-format.yml` under `not (skip_format | default(false))`, and
//! `roles/daos-storage` sets `skip_format` from inside it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::parse::{Document, Node};
use crate::references::{self, ReferenceKind};
use crate::resolve;
use crate::workspace::FileContext;

/// How far to follow includes and roles. The bug needs the mutation to be *reachable*,
/// and in practice it sits in a role's `tasks/main.yml` — one or two hops. A cap keeps
/// this off the pathological end of a diamond-shaped include graph.
const MAX_DEPTH: usize = 4;

/// How much of a playbook an import's `when:` actually covers.
///
/// The point of showing this: "runs unless skip_demo is set" says what the condition
/// decides, not how much it decides. A static paragraph about pushed-down semantics is
/// the same on every site and stops being read; a count is different every time.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Scope {
    pub plays: usize,
    /// Entries under `tasks:`/`pre_tasks:`/`post_tasks:`. Not expanded — an
    /// `include_tasks` counts as one, because that is one place the condition lands.
    pub tasks: usize,
    /// `roles:` entries. Their tasks receive the condition too, so they are named
    /// separately rather than folded into a task count that would then be wrong.
    pub roles: usize,
}

/// Count the plays, top-level tasks and roles in a playbook file.
pub fn scope_of(playbook: &Path) -> Scope {
    let Ok(text) = std::fs::read_to_string(playbook) else {
        return Scope::default();
    };
    let doc = Document::new(text);
    let Some(nodes) = doc.parse() else {
        return Scope::default();
    };
    let mut s = Scope::default();
    for doc_node in &nodes {
        for play in doc_node.items() {
            if play.get("hosts").is_none() && play.get("import_playbook").is_none() {
                continue;
            }
            s.plays += 1;
            for key in ["tasks", "pre_tasks", "post_tasks"] {
                s.tasks += play.get(key).map(|n| n.items().len()).unwrap_or(0);
            }
            s.roles += play.get("roles").map(|n| n.items().len()).unwrap_or(0);
        }
    }
    s
}

impl Scope {
    /// `None` when there is nothing worth saying.
    pub fn describe(&self) -> Option<String> {
        if self.plays == 0 {
            return None;
        }
        let plural = |n: usize, word: &str| {
            format!("{n} {word}{}", if n == 1 { "" } else { "s" })
        };
        let mut parts = Vec::new();
        if self.tasks > 0 {
            parts.push(plural(self.tasks, "task"));
        }
        if self.roles > 0 {
            parts.push(plural(self.roles, "role"));
        }
        let what = if parts.is_empty() {
            return None;
        } else {
            parts.join(" and ")
        };
        Some(format!(
            "Copied onto {what} across {}, evaluated separately at each — not a single gate.",
            plural(self.plays, "play")
        ))
    }
}

/// Variables `playbook` assigns while running, following roles and includes.
///
/// Over-collecting is the safe direction for the caller: a name here only matters if it
/// also appears in an import's condition, and missing one means missing a real bug.
pub fn mutated_vars(playbook: &Path) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut visited = HashSet::new();
    walk_file(playbook, 0, &mut out, &mut visited);
    out
}

fn walk_file(path: &Path, depth: usize, out: &mut HashSet<String>, visited: &mut HashSet<PathBuf>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(canon) = path.canonicalize() else { return };
    if !visited.insert(canon) {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else { return };
    let doc = Document::new(text);
    let Some(nodes) = doc.parse() else { return };

    for n in &nodes {
        collect_assignments(n, out);
    }

    // Follow anything that can carry a `set_fact` into this playbook's run.
    let ctx = FileContext::discover(path);
    for r in references::extract(&nodes) {
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
                // A role contributes every task file it has, not just main.yml —
                // `tasks_from` reaches the others and they set facts too.
                for f in role_task_files(&target) {
                    walk_file(&f, depth + 1, out, visited);
                }
            } else {
                walk_file(&target, depth + 1, out, visited);
            }
        }
    }
}

fn role_task_files(role_main: &Path) -> Vec<PathBuf> {
    // `resolve` hands back `<role>/tasks/main.yml`; take the whole `tasks/` dir.
    let Some(tasks_dir) = role_main.parent() else {
        return vec![role_main.to_path_buf()];
    };
    crate::workspace::yaml_files(tasks_dir)
}

/// `set_fact:` keys and `register:` values — the two ways a task changes a variable
/// mid-run. Play/role `vars:` are deliberately excluded: they're bound before the tasks
/// run, so they can't flip a condition partway through.
fn collect_assignments(node: &Node, out: &mut HashSet<String>) {
    match node {
        Node::Sequence { items, .. } => items.iter().for_each(|i| collect_assignments(i, out)),
        Node::Mapping { entries, .. } => {
            for (k, v) in entries {
                match k.as_str().map(short_key) {
                    Some("set_fact") => {
                        for (fact, _) in v.entries() {
                            if let Some(name) = fact.as_str() {
                                // `cacheable` is a set_fact option, not a fact.
                                if name != "cacheable" {
                                    out.insert(name.to_string());
                                }
                            }
                        }
                    }
                    Some("register") => {
                        if let Some(name) = v.as_str() {
                            out.insert(name.to_string());
                        }
                    }
                    _ => {}
                }
                collect_assignments(v, out);
            }
        }
        _ => {}
    }
}

fn short_key(key: &str) -> &str {
    key.rsplit('.').next().unwrap_or(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(dir: &Path, rel: &str, body: &str) -> PathBuf {
        let p = dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, body).unwrap();
        p
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ansible-lsp-mut-{name}"));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn scope_counts_plays_tasks_and_roles() {
        let d = tmp("scope");
        let pb = write(
            &d,
            "play.yml",
            "- hosts: a\n  pre_tasks:\n    - debug: {msg: 1}\n  roles:\n    - r1\n    - role: r2\n             \n  tasks:\n    - debug: {msg: 2}\n    - debug: {msg: 3}\n  post_tasks:\n    - debug: {msg: 4}\n             \n- hosts: b\n  tasks:\n    - debug: {msg: 5}\n",
        );
        let s = scope_of(&pb);
        assert_eq!(s.plays, 2);
        assert_eq!(s.tasks, 5, "pre_tasks + tasks + post_tasks across both plays");
        assert_eq!(s.roles, 2, "bare and dict forms both count");
        assert_eq!(
            s.describe().unwrap(),
            "Copied onto 5 tasks and 2 roles across 2 plays, evaluated separately at each \
             — not a single gate."
        );
    }

    #[test]
    fn scope_singularises_and_omits_empty_categories() {
        let d = tmp("scope-one");
        let pb = write(&d, "play.yml", "- hosts: a\n  tasks:\n    - debug: {msg: 1}\n");
        assert_eq!(
            scope_of(&pb).describe().unwrap(),
            "Copied onto 1 task across 1 play, evaluated separately at each — not a single gate."
        );
        // No roles mentioned when there are none.
        assert!(!scope_of(&pb).describe().unwrap().contains("role"));
    }

    /// Silence beats a wrong number: an unreadable or empty target says nothing.
    #[test]
    fn scope_declines_rather_than_guessing() {
        let d = tmp("scope-none");
        assert_eq!(scope_of(&d.join("does-not-exist.yml")).describe(), None);
        let empty = write(&d, "empty.yml", "- hosts: a\n");
        assert_eq!(scope_of(&empty).describe(), None, "a play with no tasks or roles");
        let bad = write(&d, "bad.yml", "- name: \"unterminated\n  x: [");
        assert_eq!(scope_of(&bad).describe(), None, "unparseable");
    }

    #[test]
    fn finds_set_fact_and_register_in_the_playbook_itself() {
        let d = tmp("direct");
        let pb = write(
            &d,
            "play.yml",
            "- hosts: all\n  tasks:\n    - set_fact:\n        skip_it: true\n        cacheable: yes\n\
             \n    - command: echo hi\n      register: result\n",
        );
        let got = mutated_vars(&pb);
        assert!(got.contains("skip_it"));
        assert!(got.contains("result"));
        // `cacheable` is an option of set_fact, not a fact it defines.
        assert!(!got.contains("cacheable"));
    }

    /// The real shape of the bug: the mutation is two hops away, inside a role.
    #[test]
    fn follows_roles_into_their_task_files() {
        let d = tmp("role");
        write(&d, "ansible.cfg", "[defaults]\nroles_path = ./roles\n");
        write(
            &d,
            "roles/storage/tasks/main.yml",
            "- name: mark done\n  set_fact:\n    skip_format: true\n",
        );
        let pb = write(&d, "play.yml", "- hosts: all\n  roles:\n    - storage\n");
        assert!(mutated_vars(&pb).contains("skip_format"));
    }

    #[test]
    fn follows_include_tasks() {
        let d = tmp("include");
        write(&d, "sub.yml", "- set_fact:\n    deep_var: 1\n");
        let pb = write(
            &d,
            "play.yml",
            "- hosts: all\n  tasks:\n    - include_tasks: sub.yml\n",
        );
        assert!(mutated_vars(&pb).contains("deep_var"));
    }

    /// A playbook that includes itself must not hang.
    #[test]
    fn cycles_terminate() {
        let d = tmp("cycle");
        write(&d, "b.yml", "- include_tasks: a.yml\n- set_fact:\n    from_b: 1\n");
        let pb = write(
            &d,
            "a.yml",
            "- hosts: all\n  tasks:\n    - include_tasks: b.yml\n",
        );
        assert!(mutated_vars(&pb).contains("from_b"));
    }

    /// The demo must keep demonstrating this rule. It also guards the trap that has now
    /// broken a demo file four times: an unquoted `: ` inside a task `name:` makes the
    /// file invalid YAML, which yields no references and no diagnostics — so the rule
    /// silently does nothing and looks broken. See T-013.
    #[test]
    fn demo_shows_the_mutated_condition_case() {
        let demo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../demo");
        for f in crate::workspace::yaml_files(&demo) {
            let text = std::fs::read_to_string(&f).unwrap();
            assert!(
                crate::parse::Document::new(text).parse().is_some(),
                "{} stopped parsing — check for an unquoted `: ` in a name:",
                f.display()
            );
        }
        assert!(mutated_vars(&demo.join("mutating.yml")).contains("demo_done"));
    }

    /// The real instance this rule was built for. Not a synthetic fixture — if this
    /// stops firing, either the repo was fixed or the expansion regressed.
    #[test]
    #[ignore]
    fn finds_the_real_daos_case() {
        let repo = PathBuf::from(std::env::var("HOME").unwrap()).join("matrix/ansible");
        let target = repo.join("playbooks/daos-storage-format.yml");
        if !target.exists() {
            eprintln!("skip: {} absent", target.display());
            return;
        }
        let got = mutated_vars(&target);
        assert!(
            got.contains("skip_format"),
            "daos-storage-format.yml reaches roles/daos-storage, which sets skip_format \
             — the variable playbooks/daos-deploy-full.yml:247 gates the import on. \
             Found {} names instead.",
            got.len()
        );
    }

    #[test]
    fn play_vars_are_not_mutations() {
        let d = tmp("vars");
        // Bound before tasks run, so they cannot flip a condition partway through.
        let pb = write(
            &d,
            "play.yml",
            "- hosts: all\n  vars:\n    bound_early: true\n  tasks:\n    - command: echo\n",
        );
        assert!(!mutated_vars(&pb).contains("bound_early"));
    }
}
