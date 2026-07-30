//! Which variables a playbook mutates while it runs.
//!
//! Exists for one rule. A `when:` on `import_playbook` is copied onto every task in the
//! imported playbook and evaluated per task — so if the imported playbook *sets* the
//! variable the condition reads, the condition flips partway through and the playbook
//! half-executes. Everything before the `set_fact` runs, everything after silently
//! skips, and `set_fact` is host-scoped so a cluster can split.
//!
//! Found in the real repo: `playbooks/lustre-deploy-full.yml` imports
//! `lustre-storage-format.yml` under `not (skip_format | default(false))`, and
//! `roles/lustre-storage` sets `skip_format` from inside it.

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
            // `unparseable*.yml` are broken on purpose — the fixtures for the T-013 hint.
            if f.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("unparseable")) {
                continue;
            }
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
    fn finds_the_real_lustre_case() {
        let repo = PathBuf::from(std::env::var("HOME").unwrap()).join("app/ansible");
        let target = repo.join("playbooks/lustre-storage-format.yml");
        if !target.exists() {
            eprintln!("skip: {} absent", target.display());
            return;
        }
        let got = mutated_vars(&target);
        assert!(
            got.contains("skip_format"),
            "lustre-storage-format.yml reaches roles/lustre-storage, which sets skip_format \
             — the variable playbooks/lustre-deploy-full.yml:247 gates the import on. \
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
