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

use crate::ast::{self, Ast, PlayItem, Stmt, Task};
use crate::config::{AnsibleConfig, EnvMap};
use crate::fs::{Fs, StdFs};
use crate::parse::Document;
use crate::references::{self, ReferenceKind};
use crate::resolve;
use crate::workspace::FileContext;

/// Variables `playbook` assigns while running, following roles and includes.
///
/// Over-collecting is the safe direction for the caller: a name here only matters if it
/// also appears in an import's condition, and missing one means missing a real bug.
pub fn mutated_vars(playbook: &Path) -> HashSet<String> {
    mutated_vars_in(playbook, &StdFs, &EnvMap::from_process())
}

/// [`mutated_vars`] against a caller-supplied filesystem, so a scan's memo covers this walk
/// too (T-085), and a caller-supplied environment.
pub fn mutated_vars_in(playbook: &Path, fs: &dyn Fs, env: &EnvMap) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut visited = HashSet::new();
    walk_file(playbook, fs, env, &mut out, &mut visited);
    out
}

fn walk_file(
    path: &Path,
    fs: &dyn Fs,
    env: &EnvMap,
    out: &mut HashSet<String>,
    visited: &mut HashSet<PathBuf>,
) {
    let Some(canon) = fs.canonical(path) else { return };
    if !visited.insert(canon) {
        return;
    }
    let Some(text) = fs.read(path) else { return };
    let doc = Document::new(text);
    let Some(nodes) = doc.parse() else { return };

    collect_assignments(&ast::build(&nodes), out);

    // Follow anything that can carry a `set_fact` into this playbook's run.
    let ctx = FileContext::discover_with(path, fs, |root| {
        AnsibleConfig::builder(root).fs(fs).env(env).load()
    });
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
        for target in resolve::resolve_in(&r, &ctx, fs).targets {
            if r.kind == ReferenceKind::Role {
                // A role contributes every task file it has, not just main.yml —
                // `tasks_from` reaches the others and they set facts too.
                for f in role_task_files(&target) {
                    walk_file(&f, fs, env, out, visited);
                }
            } else {
                walk_file(&target, fs, env, out, visited);
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
fn collect_assignments(tree: &Ast, out: &mut HashSet<String>) {
    match tree {
        Ast::Playbook(items) => {
            for it in items {
                if let PlayItem::Play(p) = it {
                    for s in p
                        .pre_tasks
                        .iter()
                        .chain(&p.tasks)
                        .chain(&p.post_tasks)
                        .chain(&p.handlers)
                    {
                        collect_stmt(s, out);
                    }
                }
            }
        }
        Ast::Tasks(stmts) => stmts.iter().for_each(|s| collect_stmt(s, out)),
        Ast::Other => {}
    }
}

fn collect_stmt(s: &Stmt, out: &mut HashSet<String>) {
    match s {
        Stmt::Task(t) => collect_task(t, out),
        Stmt::Block(b) => b
            .block
            .iter()
            .chain(&b.rescue)
            .chain(&b.always)
            .for_each(|s| collect_stmt(s, out)),
    }
}

fn collect_task(t: &Task, out: &mut HashSet<String>) {
    if let Some(a) = &t.action {
        if crate::keywords::core_action(&a.name) == "set_fact" {
            for (fact, _) in a.args.entries() {
                if let Some(name) = fact.as_str() {
                    // `cacheable` is a set_fact option, not a fact.
                    if name != "cacheable" {
                        out.insert(name.to_string());
                    }
                }
            }
        }
    }
    if let Some(reg) = &t.register {
        out.insert(reg.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// [`mutated_vars`] with an empty environment, so a fixture's `ansible.cfg` can't be
    /// overridden by whatever the invoking shell exports.
    fn mutated(pb: &Path) -> HashSet<String> {
        mutated_vars_in(pb, &StdFs, &EnvMap::empty())
    }

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
            r#"
            - hosts: all
              tasks:
                - set_fact:
                    skip_it: true
                    cacheable: yes

                - command: echo hi
                  register: result
"#,
        );
        let got = mutated(&pb);
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
            r#"
            - name: mark done
              set_fact:
                skip_format: true
"#,
        );
        let pb = write(
            &d,
            "play.yml",
            r#"
            - hosts: all
              roles:
                - storage
"#,
        );
        assert!(mutated(&pb).contains("skip_format"));
    }

    #[test]
    fn follows_include_tasks() {
        let d = tmp("include");
        write(
            &d,
            "sub.yml",
            r#"
            - set_fact:
                deep_var: 1
"#,
        );
        let pb = write(
            &d,
            "play.yml",
            r#"
            - hosts: all
              tasks:
                - include_tasks: sub.yml
"#,
        );
        assert!(mutated(&pb).contains("deep_var"));
    }

    /// A playbook that includes itself must not hang.
    #[test]
    fn cycles_terminate() {
        let d = tmp("cycle");
        write(
            &d,
            "b.yml",
            r#"
            - include_tasks: a.yml
            - set_fact:
                from_b: 1
"#,
        );
        let pb = write(
            &d,
            "a.yml",
            r#"
            - hosts: all
              tasks:
                - include_tasks: b.yml
"#,
        );
        assert!(mutated(&pb).contains("from_b"));
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
        assert!(mutated(&demo.join("mutating.yml")).contains("demo_done"));
    }

    /// A real instance, taken from upstream rather than invented. The chain is
    /// `ansible.posix`'s selinux target (`tests/integration/targets/selinux/tasks/`, commit
    /// `ffdf9ef`): `main.yml` guards an `include_tasks: selinux.yml` behind a `when:`, and
    /// the included file then `set_fact`s and `register`s several names.
    ///
    /// That is the shape the rule exists for — the mutation is invisible from the entry
    /// point, so anything that stops following the include silently reports nothing. It
    /// replaces a test pinned to one private repo under `$HOME` (T-077), which skipped
    /// everywhere else and panicked outright on Windows.
    ///
    /// Written out here rather than vendored: the upstream file is ~140 lines of GPL-3.0
    /// test code, and only its include/`set_fact` skeleton is load-bearing.
    #[test]
    fn follows_a_real_upstream_include_chain() {
        let d = tmp("posix-selinux");
        // Raw strings, indented with the code: YAML tolerates a uniformly indented root node,
        // so the fixture reads as the file it stands for instead of as escape soup.
        write(
            &d,
            "selinux.yml",
            r#"
            - name: Get current SELinux config
              ansible.builtin.slurp:
                src: /etc/sysconfig/selinux
              register: selinux_config_original_base64

            - name: Decode the config
              ansible.builtin.set_fact:
                selinux_config_original_raw: "{{ selinux_config_original_base64.content | b64decode }}"
                before_test_sestatus: "{{ ansible_selinux }}"
"#,
        );
        let pb = write(
            &d,
            "main.yml",
            r#"
            - hosts: all
              tasks:
                - name: Include_tasks for when SELinux is enabled
                  ansible.builtin.include_tasks: selinux.yml
                  when:
                    - ansible_selinux is defined
                    - ansible_selinux.status == 'enabled'
"#,
        );

        let got = mutated(&pb);
        // The `register:`, and both names from the one `set_fact:` — a mapping with several
        // keys defines all of them, not just the first.
        assert!(got.contains("selinux_config_original_base64"), "register, found {got:?}");
        assert!(got.contains("selinux_config_original_raw"), "set_fact, found {got:?}");
        assert!(got.contains("before_test_sestatus"), "second set_fact key, found {got:?}");
        // `src:` and `content` are module arguments and a lookup, not mutated variables.
        assert!(!got.contains("src"));
        assert!(!got.contains("content"));
    }

    #[test]
    fn play_vars_are_not_mutations() {
        let d = tmp("vars");
        // Bound before tasks run, so they cannot flip a condition partway through.
        let pb = write(
            &d,
            "play.yml",
            r#"
            - hosts: all
              vars:
                bound_early: true
              tasks:
                - command: echo
"#,
        );
        assert!(!mutated(&pb).contains("bound_early"));
    }
}
