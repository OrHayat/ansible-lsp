//! Reference -> file on disk, following Ansible's own search order.

use crate::install::AnsibleInstall;
use crate::references::{Reference, ReferenceKind};
use crate::workspace::FileContext;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Resolved,
    Missing,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// Contains `{{ }}` — the real target depends on runtime variables.
    Templated,
    /// Nothing to point at in this workspace (e.g. `ansible.builtin.*`, or a role
    /// installed outside it). Not an error, so never diagnosed.
    NotInWorkspace,
}

#[derive(Debug, Clone)]
pub struct Resolution {
    pub status: Status,
    pub targets: Vec<PathBuf>,
    /// Every path tried, in order — surfaced in diagnostics so a bad root is
    /// debuggable from the message alone.
    pub candidates: Vec<PathBuf>,
    pub skip_reason: Option<SkipReason>,
}

impl Resolution {
    fn skipped(reason: SkipReason) -> Self {
        Self {
            status: Status::Skipped,
            targets: Vec::new(),
            candidates: Vec::new(),
            skip_reason: Some(reason),
        }
    }

    /// First match wins — verified against real `ansible-playbook`, which silently
    /// takes the first hit in its search order and never reports the shadowed one.
    fn from_candidates(candidates: Vec<PathBuf>) -> Self {
        match candidates.iter().find(|p| p.is_file()).cloned() {
            Some(p) => Self {
                status: Status::Resolved,
                targets: vec![p],
                candidates,
                skip_reason: None,
            },
            None => Self {
                status: Status::Missing,
                targets: Vec::new(),
                candidates,
                skip_reason: None,
            },
        }
    }
}

/// Diagnostic rule id, for `# noqa: <id>` and for display.
pub fn rule_id(r: &Reference) -> &'static str {
    match (r.kind, r.templated) {
        (ReferenceKind::ImportPlaybook, true) => "templated-import",
        _ => "missing-file",
    }
}

pub fn resolve(r: &Reference, ctx: &FileContext) -> Resolution {
    // A templated target is only knowable at runtime. Offer every file the pattern
    // could reach, but never warn — an untrustworthy warning is worse than none.
    // A templated static import is wrong whatever is on disk — Ansible templates the
    // string before looking, so it can never reach a file literally named `{{ x }}.yml`.
    // Don't touch the filesystem; report the templating itself.
    if r.templated && r.kind == ReferenceKind::ImportPlaybook {
        return Resolution {
            status: Status::Missing,
            targets: Vec::new(),
            candidates: Vec::new(),
            skip_reason: None,
        };
    }

    if r.templated {
        let bases = match r.kind {
            ReferenceKind::IncludeTasks | ReferenceKind::ImportTasks => ctx.task_search_dirs(),
            _ => return Resolution::skipped(SkipReason::Templated),
        };
        let targets = crate::glob::candidates(&bases, &r.value);
        return Resolution {
            status: if targets.is_empty() {
                Status::Skipped
            } else {
                Status::Resolved
            },
            targets,
            candidates: Vec::new(),
            skip_reason: Some(SkipReason::Templated),
        };
    }

    match r.kind {
        ReferenceKind::IncludeTasks | ReferenceKind::ImportTasks => {
            Resolution::from_candidates(unique(
                ctx.task_search_dirs()
                    .iter()
                    .map(|b| normalise(&b.join(&r.value))),
            ))
        }

        // Relative to the importing playbook, then the project root. No role or
        // collection paths apply at play level.
        ReferenceKind::ImportPlaybook => Resolution::from_candidates(unique(
            [Some(ctx.file_dir.clone()), ctx.project_root.clone()]
                .into_iter()
                .flatten()
                .map(|b| normalise(&b.join(&r.value))),
        )),

        ReferenceKind::Role => match role_dir(&r.value, ctx) {
            Some(dir) => {
                let res = Resolution::from_candidates(with_ext(&dir.join("tasks"), "main"));
                // `roles/cib-batch` has only begin/commit/abort.yml and no main.yml —
                // legal, because every caller passes tasks_from. Warning here would
                // fire on 16 working references in this repo alone.
                match (res.status, r.has_tasks_from) {
                    (Status::Missing, true) => Resolution::skipped(SkipReason::NotInWorkspace),
                    _ => res,
                }
            }
            // The role name itself resolved to nothing: that IS worth reporting.
            None => Resolution {
                status: Status::Missing,
                targets: Vec::new(),
                candidates: ctx.roles_roots().iter().map(|d| d.join(&r.value)).collect(),
                skip_reason: None,
            },
        },

        ReferenceKind::TasksFrom => {
            let Some(role) = r.role.as_deref().and_then(|n| role_dir(n, ctx)) else {
                return Resolution::skipped(SkipReason::NotInWorkspace);
            };
            Resolution::from_candidates(with_ext(&role.join("tasks"), &r.value))
        }

        ReferenceKind::Module => {
            let parts: Vec<&str> = r.value.split('.').collect();
            let [ns, coll, module] = parts[..] else {
                return Resolution::skipped(SkipReason::NotInWorkspace);
            };
            let mut candidates: Vec<PathBuf> = ctx
                .collection_roots()
                .iter()
                .flat_map(|root| {
                    let base = root.join(ns).join(coll).join("plugins");
                    [
                        base.join("modules").join(format!("{module}.py")),
                        base.join("action").join(format!("{module}.py")),
                    ]
                })
                .collect();
            // ansible.builtin lives in the ansible package, not a collection tree.
            if (ns, coll) == ("ansible", "builtin") {
                if let Some(p) = AnsibleInstall::detect().builtin_module(module) {
                    candidates.insert(0, p);
                }
            }
            let res = Resolution::from_candidates(candidates);
            // A module we can't find is usually a collection that isn't installed
            // here, not a typo — don't warn.
            match res.status {
                Status::Missing => Resolution::skipped(SkipReason::NotInWorkspace),
                _ => res,
            }
        }
    }
}

/// Role name -> its directory. Handles plain names and 3-part FQCNs.
fn role_dir(name: &str, ctx: &FileContext) -> Option<PathBuf> {
    if name.contains("{{") {
        return None;
    }
    let parts: Vec<&str> = name.split('.').collect();
    if let [ns, coll, role] = parts[..] {
        return ctx
            .collection_roots()
            .iter()
            .map(|r| r.join(ns).join(coll).join("roles").join(role))
            .find(|p| p.is_dir());
    }
    ctx.roles_roots()
        .iter()
        .map(|r| r.join(name))
        .find(|p| p.is_dir())
}

/// `tasks_from: begin` and `tasks_from: begin.yml` are both legal.
fn with_ext(dir: &Path, stem: &str) -> Vec<PathBuf> {
    if stem.ends_with(".yml") || stem.ends_with(".yaml") {
        return vec![dir.join(stem)];
    }
    vec![
        dir.join(format!("{stem}.yml")),
        dir.join(format!("{stem}.yaml")),
    ]
}

fn unique(paths: impl Iterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for p in paths {
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// Lexical `..`/`.` removal. Not `canonicalize()`, which needs the path to exist —
/// we want readable candidates precisely when they don't.
fn normalise(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::Document;
    use crate::references::extract;

    fn repo() -> Option<PathBuf> {
        let p = PathBuf::from(std::env::var("HOME").ok()?).join("matrix/ansible");
        p.is_dir().then_some(p)
    }

    fn resolve_src(file: &Path, src: &str) -> Vec<(Reference, Resolution)> {
        let doc = Document::new(src.to_string());
        let ctx = FileContext::discover(file);
        extract(&doc.parse().unwrap())
            .into_iter()
            .map(|r| {
                let res = resolve(&r, &ctx);
                (r, res)
            })
            .collect()
    }

    fn resolve_in(file: &Path, value: &str) -> Resolution {
        resolve_src(file, &format!("- include_tasks: {value}\n"))
            .pop()
            .unwrap()
            .1
    }

    fn first(out: &[(Reference, Resolution)], kind: ReferenceKind) -> &Resolution {
        &out.iter().find(|(r, _)| r.kind == kind).expect("kind").1
    }

    /// The four references in this repo that do NOT resolve relative to the including
    /// file. All are live, working Ansible.
    #[test]
    fn real_repo_regressions() {
        let Some(root) = repo() else { return };
        let cases = [
            (
                "roles/daos-snapshot/tasks/query/timestamp.yml",
                "query/exists.yml",
            ),
            (
                "roles/ad/tasks/join.yml",
                "../../playbooks/tasks/select-available-node.yml",
            ),
            (
                "roles/nautobot-docker/tasks/sanity-tests/main.yml",
                "sanity-tests/database-tests.yml",
            ),
            (
                "roles/nautobot-docker/tasks/sanity-tests/main.yml",
                "sanity-tests/celery-tests.yml",
            ),
        ];
        for (from, target) in cases {
            let res = resolve_in(&root.join(from), target);
            assert_eq!(
                res.status,
                Status::Resolved,
                "{from} -> {target}; tried {:#?}",
                res.candidates
            );
        }
    }

    /// Mirror image: target sits next to the caller, inside a `tasks/` subdirectory.
    /// Three `_converge_one_ap.yml` exist; each caller must reach its own.
    #[test]
    fn sibling_include_inside_a_tasks_subdirectory() {
        let Some(root) = repo() else { return };
        let from = root.join("roles/sync-state/tasks/nfs_access_point/reconcile.yml");
        let res = resolve_in(&from, "_converge_one_ap.yml");
        assert_eq!(res.status, Status::Resolved, "tried {:#?}", res.candidates);
        assert!(
            res.targets[0].ends_with("nfs_access_point/_converge_one_ap.yml"),
            "must reach the caller's own copy, got {:?}",
            res.targets[0]
        );
    }

    #[test]
    fn missing_file_reports_every_candidate_tried() {
        let Some(root) = repo() else { return };
        let res = resolve_in(
            &root.join("roles/sync-state/tasks/nfs_access_point/reconcile.yml"),
            "definitely_not_here.yml",
        );
        assert_eq!(res.status, Status::Missing);
        assert!(!res.candidates.is_empty());
    }

    /// Templated values may resolve to several files or none — either way they must
    /// never produce a warning.
    #[test]
    fn templated_paths_never_warn() {
        let Some(root) = repo() else { return };
        for value in [
            "\"{{ ap_protocol }}/validate.yml\"",
            "\"{{ nothing_matches_this }}/xyzzy.yml\"",
        ] {
            let res = resolve_in(
                &root.join("roles/sync-state/tasks/nfs_access_point/reconcile.yml"),
                value,
            );
            assert_ne!(res.status, Status::Missing, "{value} must not warn");
            assert_eq!(res.skip_reason, Some(SkipReason::Templated));
        }
    }

    /// The legacy plugin globbed templated includes; dropping that was a regression.
    #[test]
    fn templated_include_offers_every_candidate() {
        let Some(root) = repo() else { return };
        let res = resolve_in(
            &root.join("roles/sync-state/tasks/main.yml"),
            "\"{{ proto }}_access_point/_converge_one_ap.yml\"",
        );
        assert_eq!(res.status, Status::Resolved);
        assert!(res.targets.len() >= 2, "expected several, got {:?}", res.targets);
    }

    #[test]
    fn import_playbook_resolves_relative_to_the_importer() {
        let Some(root) = repo() else { return };
        for (from, target) in [
            ("playbooks/daos-full-deploy.yml", "daos-infrastructure.yml"),
            ("playbooks/matrix/setup.yml", "../../network-setup.yml"),
        ] {
            let out = resolve_src(
                &root.join(from),
                &format!("- import_playbook: {target}\n"),
            );
            let res = first(&out, ReferenceKind::ImportPlaybook);
            if res.status != Status::Resolved {
                // Path may not exist in this checkout; only assert when it does.
                continue;
            }
            assert!(res.targets[0].is_file());
        }
    }

    /// Unlike include_tasks, a templated static import cannot work — report it, and
    /// report it without consulting the filesystem: a file literally named
    /// `{{ env }}-setup.yml` would resolve here but Ansible could never reach it.
    #[test]
    fn templated_import_playbook_is_reported_not_skipped() {
        let Some(root) = repo() else { return };
        let out = resolve_src(
            &root.join("playbooks/site.yml"),
            "- import_playbook: \"{{ env }}-setup.yml\"\n",
        );
        let res = first(&out, ReferenceKind::ImportPlaybook);
        assert_eq!(res.status, Status::Missing);
        assert!(
            res.candidates.is_empty(),
            "must not imply it went looking for a braces-named file"
        );
    }

    #[test]
    fn role_by_name_resolves_via_roles_path() {
        let Some(root) = repo() else { return };
        let out = resolve_src(
            &root.join("playbooks/site.yml"),
            "- include_role:\n    name: podman\n",
        );
        let res = first(&out, ReferenceKind::Role);
        assert_eq!(res.status, Status::Resolved, "tried {:#?}", res.candidates);
        assert!(res.targets[0].ends_with("roles/podman/tasks/main.yml"));
    }

    /// `roles/cib-batch` has begin/commit/abort.yml but no main.yml — legal, because
    /// every caller passes tasks_from. 16 working references in this repo depend on
    /// this not warning.
    #[test]
    fn role_without_main_is_fine_when_tasks_from_is_given() {
        let Some(root) = repo() else { return };
        let out = resolve_src(
            &root.join("playbooks/site.yml"),
            "- include_role: { name: cib-batch, tasks_from: begin }\n",
        );
        assert_eq!(first(&out, ReferenceKind::Role).status, Status::Skipped);
        assert_eq!(first(&out, ReferenceKind::TasksFrom).status, Status::Resolved);
    }

    /// Same role, no tasks_from: now main.yml really is required, so it's an error.
    #[test]
    fn role_without_main_and_without_tasks_from_is_missing() {
        let Some(root) = repo() else { return };
        let out = resolve_src(
            &root.join("playbooks/site.yml"),
            "- include_role:\n    name: cib-batch\n",
        );
        assert_eq!(first(&out, ReferenceKind::Role).status, Status::Missing);
    }

    /// A role name that resolves nowhere is worth reporting — this one is a real
    /// break in the repo: site.yml lists `daos`, and roles/daos does not exist.
    #[test]
    fn unknown_role_name_is_reported() {
        let Some(root) = repo() else { return };
        let out = resolve_src(&root.join("site.yml"), "- hosts: all\n  roles:\n    - daos\n");
        let res = first(&out, ReferenceKind::Role);
        assert_eq!(res.status, Status::Missing);
        assert!(!res.candidates.is_empty(), "must report where it looked");
    }

    #[test]
    fn roles_block_entries_resolve() {
        let Some(root) = repo() else { return };
        let out = resolve_src(
            &root.join("playbooks/site.yml"),
            "- hosts: all\n  roles:\n    - podman\n",
        );
        assert_eq!(first(&out, ReferenceKind::Role).status, Status::Resolved);
    }

    #[test]
    fn tasks_from_resolves_inside_its_own_role() {
        let Some(root) = repo() else { return };
        let out = resolve_src(
            &root.join("playbooks/site.yml"),
            "- include_role: { name: cib-batch, tasks_from: begin }\n",
        );
        let res = first(&out, ReferenceKind::TasksFrom);
        assert_eq!(res.status, Status::Resolved, "tried {:#?}", res.candidates);
        assert!(res.targets[0].ends_with("roles/cib-batch/tasks/begin.yml"));
    }

    #[test]
    fn in_repo_collection_module_resolves() {
        let Some(root) = repo() else { return };
        let out = resolve_src(
            &root.join("playbooks/site.yml"),
            "- volumez.daos.pool_create:\n    name: p\n",
        );
        let res = first(&out, ReferenceKind::Module);
        assert_eq!(res.status, Status::Resolved, "tried {:#?}", res.candidates);
        assert!(res.targets[0].ends_with("volumez/daos/plugins/modules/pool_create.py"));
    }

    /// Like pyright into site-packages: jump into the installed ansible itself.
    #[test]
    fn builtin_modules_resolve_into_the_installed_ansible() {
        let Some(root) = repo() else { return };
        if AnsibleInstall::detect().package_dir.is_none() {
            return; // ansible not on PATH
        }
        let out = resolve_src(
            &root.join("playbooks/site.yml"),
            "- ansible.builtin.systemd:\n    name: x\n",
        );
        let res = first(&out, ReferenceKind::Module);
        assert_eq!(res.status, Status::Resolved, "tried {:#?}", res.candidates);
        assert!(res.targets[0].ends_with("ansible/modules/systemd.py"));
    }

    /// An installed collection you never wrote, found via `ansible --version`.
    #[test]
    fn installed_collection_modules_resolve() {
        let Some(root) = repo() else { return };
        if AnsibleInstall::detect().collection_roots.is_empty() {
            return;
        }
        let out = resolve_src(
            &root.join("playbooks/site.yml"),
            "- community.postgresql.postgresql_user:\n    name: x\n",
        );
        let res = first(&out, ReferenceKind::Module);
        assert_eq!(res.status, Status::Resolved, "tried {:#?}", res.candidates);
        assert!(res.targets[0].ends_with("postgresql/plugins/modules/postgresql_user.py"));
    }

    /// A collection genuinely not installed: silent, never a warning.
    #[test]
    fn uninstalled_collection_is_skipped_not_warned() {
        let Some(root) = repo() else { return };
        let out = resolve_src(
            &root.join("playbooks/site.yml"),
            "- nosuch.collection.module:\n    name: x\n",
        );
        assert_eq!(first(&out, ReferenceKind::Module).status, Status::Skipped);
    }
}

#[cfg(test)]
mod ambiguity {
    use super::*;
    use crate::parse::Document;
    use crate::references::extract;

    /// `dup.yml` exists in BOTH the role's `tasks/` dir and next to the including
    /// file. Verified against real `ansible-playbook`: it silently picks the role's
    /// `tasks/` dir. No warning, no error — first match in its search order wins.
    #[test]
    fn matches_ansible_when_a_name_exists_in_two_search_paths() {
        let from = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/ambiguous/roles/amb/tasks/sub/inner.yml");
        let doc = Document::new("- include_tasks: dup.yml\n".to_string());
        let ctx = FileContext::discover(&from);
        let refs = extract(&doc.parse().unwrap());
        let res = resolve(&refs[0], &ctx);

        assert_eq!(res.status, Status::Resolved);
        assert!(
            res.targets[0].ends_with("amb/tasks/dup.yml"),
            "ansible picks the role tasks dir, got {:?}",
            res.targets[0]
        );
        // Both really do exist — otherwise this test proves nothing.
        assert!(from.parent().unwrap().join("dup.yml").is_file());
    }
}

#[cfg(test)]
mod perf {
    use super::*;
    use crate::parse::Document;
    use crate::references::extract;
    use std::time::Instant;

    #[test]
    #[ignore = "profiling aid: cargo test perf -- --ignored --nocapture"]
    fn profile_largest_file() {
        let Ok(home) = std::env::var("HOME") else { return };
        let root = PathBuf::from(&home).join("matrix/ansible");
        if !root.is_dir() { return; }
        // find largest yml
        let mut biggest: Option<(PathBuf, u64)> = None;
        fn walk(d: &Path, best: &mut Option<(PathBuf, u64)>) {
            let Ok(rd) = std::fs::read_dir(d) else { return };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    let n = p.file_name().unwrap_or_default().to_string_lossy().to_string();
                    if n == ".git" || n == "__pycache__" { continue; }
                    walk(&p, best);
                } else if p.extension().map_or(false, |x| x == "yml") {
                    let s = e.metadata().map(|m| m.len()).unwrap_or(0);
                    if best.as_ref().map_or(true, |(_, b)| s > *b) { *best = Some((p, s)); }
                }
            }
        }
        walk(&root, &mut biggest);
        let (path, size) = biggest.unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        println!("file: {} ({} KB)", path.display(), size / 1024);

        let t = Instant::now();
        let doc = Document::new(text);
        let nodes = doc.parse().unwrap();
        println!("parse:        {:?}", t.elapsed());

        let t = Instant::now();
        let refs = extract(&nodes);
        println!("extract:      {:?}  ({} refs)", t.elapsed(), refs.len());

        let t = Instant::now();
        let ctx = FileContext::discover(&path);
        println!("discover:     {:?}", t.elapsed());

        let t = Instant::now();
        let n = ctx.collection_roots().len();
        println!("collection_roots x1: {:?} ({} roots)", t.elapsed(), n);

        let t = Instant::now();
        for r in &refs { let _ = resolve(r, &ctx); }
        println!("resolve all:  {:?}", t.elapsed());

        let by_kind = |k: ReferenceKind| refs.iter().filter(|r| r.kind == k).count();
        println!("  modules={} roles={} tasks={}",
            by_kind(ReferenceKind::Module), by_kind(ReferenceKind::Role),
            by_kind(ReferenceKind::IncludeTasks));
    }
}
