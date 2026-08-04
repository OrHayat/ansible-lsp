//! Reference -> file on disk, following Ansible's own search order.

use crate::fs::{Fs, StdFs};
use crate::include_vars;
use crate::install::AnsibleInstall;
use crate::references::{Reference, ReferenceKind};
use crate::workspace::FileContext;
use std::collections::HashMap;
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
    fn from_candidates(candidates: Vec<PathBuf>, fs: &dyn Fs) -> Self {
        match candidates.iter().find(|p| fs.is_file(p)).cloned() {
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
        (ReferenceKind::IncludeVarsDir, _) => "missing-dir",
        _ => "missing-file",
    }
}

/// Expand the magic variables whose value we already know, into every plausible literal.
///
/// Returns `(expansions, still_templated)`. `still_templated` means at least one `{{ }}`
/// survived, so the caller must glob rather than diagnose.
///
/// `role_path` is NOT expanded here anymore. The old claim — "Ansible defines it as the
/// directory of the role containing the task, which is exactly `FileContext::role_dir`" —
/// is wrong: Ansible injects it per task from whichever role *invoked* the task
/// (`vars/manager.py:478-481`), so it can be undefined (no role chain → runtime crash) or
/// a different role's dir (cross-role include). `role_dir` is a folder-shape guess, and
/// warnings built on it can lie. Disabled until T-068 derives the value from invocation
/// chains (needs T-020); the search-order half of the problem is T-067. Cost, accepted
/// knowingly: the 4 `~/app/ansible` references that expansion made navigable fall back
/// to globbing, and `{{ role_path }}` misses no longer warn.
///
/// `playbook_dir` is different and taught the lesson the hard way: substituting it with
/// the project root produced 4 false "missing file" warnings, because the playbooks live
/// in `<root>/playbooks/` and the references read `{{ playbook_dir }}/../roles/...`.
/// Which playbook is running is genuinely a runtime fact, so it expands to *several*
/// candidates and a hit on any of them counts. Guessing one would be a false positive
/// generator, and this resolver's whole value rests on not producing those.
/// The third element says whether anything was substituted. That matters: an expanded
/// `{{ role_path }}/x.yml` is a COMPLETE path, so it must not then be joined onto the
/// task search dirs. Joining only looked correct while the roots happened to be absolute
/// — with a relative root it appended, producing `demo/tasks/demo/tasks/x.yml`.
fn expand_magic(value: &str, ctx: &FileContext) -> (Vec<String>, bool, bool) {
    let mut out = vec![value.to_string()];

    let apply = |name: &str, dirs: Vec<PathBuf>, out: &mut Vec<String>| {
        let forms = [format!("{{{{ {name} }}}}"), format!("{{{{{name}}}}}")];
        if !out
            .iter()
            .any(|v| forms.iter().any(|f| v.contains(f.as_str())))
        {
            return;
        }
        let replacements: Vec<String> = dirs
            .iter()
            .filter_map(|d| d.to_str().map(str::to_owned))
            .collect();
        if replacements.is_empty() {
            return;
        }
        let mut expanded = Vec::new();
        for v in out.iter() {
            for d in &replacements {
                let mut s = v.clone();
                for f in &forms {
                    s = s.replace(f.as_str(), d);
                }
                expanded.push(s);
            }
        }
        *out = expanded;
    };

    // `role_path` deliberately not substituted — see the doc comment above (T-067/T-068).
    // Left templated, it falls through to the glob path and can never produce a warning.

    // Ambiguous. The project root covers a top-level playbook; `<root>/playbooks` covers
    // the convention this repo actually uses.
    let playbook_dirs: Vec<PathBuf> = ctx
        .project_root
        .iter()
        .flat_map(|r| [r.clone(), r.join("playbooks")])
        .collect();
    apply("playbook_dir", playbook_dirs, &mut out);
    // `inventory_dir` deliberately not substituted. It is per-host — the directory of the
    // inventory source that first defined the host (`inventory/data.py:197-202`), set by
    // `-i`/ansible.cfg at launch, `None` for add_host hosts — nothing like a playbook dir.
    // Borrowing the playbook guesses for it was wrong on both value and definedness; T-070
    // derives it from real inventory sources. Left templated, it globs and never warns.

    let still_templated = out.iter().any(|v| v.contains("{{"));
    let substituted = out.len() != 1 || out[0] != value;
    (out, still_templated, substituted)
}

/// Like [`resolve`], but first substitutes `{{ var }}` tokens with a variable's known-literal
/// value (T-056), so a path like `{{ env }}.yml` becomes navigable when `env` is knowable.
/// Navigation only: the result is marked templated so it is NEVER warned about — a variable
/// can be overridden at runtime by `-e`, so a "missing" here would be a false certainty.
pub fn resolve_with(
    r: &Reference,
    ctx: &FileContext,
    literals: &HashMap<String, Vec<String>>,
) -> Resolution {
    resolve_with_in(r, ctx, literals, &StdFs)
}

/// [`resolve_with`] against a caller-supplied filesystem, so a scan can memoize the probes
/// (T-085).
pub fn resolve_with_in(
    r: &Reference,
    ctx: &FileContext,
    literals: &HashMap<String, Vec<String>>,
    fs: &dyn Fs,
) -> Resolution {
    if r.value.contains("{{") {
        if let Some(bases) = path_bases(r.kind, ctx) {
            let subs = substitute_literals(&r.value, literals);
            if !subs.is_empty() {
                let mut cands = Vec::new();
                for v in &subs {
                    for b in &bases {
                        cands.push(normalise(&b.join(v)));
                    }
                }
                let mut res = Resolution::from_candidates(unique(cands.into_iter()), fs);
                // Offer, don't assert: navigable, but never a warning.
                res.skip_reason = Some(SkipReason::Templated);
                if res.status == Status::Missing {
                    res.status = Status::Skipped;
                }
                return res;
            }
        }
    }
    resolve_in(r, ctx, fs)
}

/// The base directories a path-shaped reference is resolved against. `None` for name-shaped
/// kinds (role/module/tasks_from), which aren't file paths to substitute into.
fn path_bases(kind: ReferenceKind, ctx: &FileContext) -> Option<Vec<PathBuf>> {
    match kind {
        ReferenceKind::IncludeTasks | ReferenceKind::ImportTasks => Some(ctx.task_search_dirs()),
        ReferenceKind::ImportPlaybook => Some(
            [Some(ctx.file_dir.clone()), ctx.project_root.clone()]
                .into_iter()
                .flatten()
                .collect(),
        ),
        ReferenceKind::IncludeVars => {
            let mut b = vec![ctx.file_dir.clone(), ctx.file_dir.join("vars")];
            if let Some(role) = &ctx.role_dir {
                b.push(role.join("vars"));
            }
            if let Some(root) = &ctx.project_root {
                b.push(root.clone());
            }
            Some(b)
        }
        _ => None,
    }
}

/// Substitute every `{{ var }}` in `value` with the variable's literal value(s). A token must
/// be a bare identifier with a known literal; anything else (a filter, an unknown var) makes
/// the whole value unresolvable — return empty, so the caller falls back to normal handling.
/// Several values for one variable produce several results (a candidate each).
fn substitute_literals(value: &str, literals: &HashMap<String, Vec<String>>) -> Vec<String> {
    let mut results = vec![String::new()];
    let mut rest = value;
    loop {
        let Some(open) = rest.find("{{") else { break };
        let prefix = &rest[..open];
        let after = &rest[open + 2..];
        let Some(close) = after.find("}}") else {
            return Vec::new();
        };
        let token = after[..close].trim();
        if token.is_empty() || !token.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Vec::new();
        }
        let Some(vals) = literals.get(token) else {
            return Vec::new();
        };
        let mut next = Vec::new();
        for base in &results {
            for v in vals {
                next.push(format!("{base}{prefix}{v}"));
            }
        }
        results = next;
        rest = &after[close + 2..];
    }
    for r in &mut results {
        r.push_str(rest);
    }
    results.sort();
    results.dedup();
    results
}

pub fn resolve(r: &Reference, ctx: &FileContext) -> Resolution {
    resolve_in(r, ctx, &StdFs)
}

/// [`resolve`] against a caller-supplied filesystem. Role search alone re-probes the same
/// name against the same roots once per consuming file, so a memoizing `fs` is most of
/// T-085's win.
pub fn resolve_in(r: &Reference, ctx: &FileContext, fs: &dyn Fs) -> Resolution {
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

    // `{{ role_path }}` and friends are known here, so substitute before deciding this
    // is unknowable. A value that becomes fully literal is then resolved — and diagnosed
    // — like any other path.
    let (values, templated, substituted) = expand_magic(&r.value, ctx);

    if templated {
        let bases = match r.kind {
            ReferenceKind::IncludeTasks | ReferenceKind::ImportTasks => ctx.task_search_dirs(),
            _ => return Resolution::skipped(SkipReason::Templated),
        };
        let targets = crate::glob::candidates_in(&bases, &r.value, fs);
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
            // An expansion is already anchored at the directory it named, so the search
            // path does not apply to it.
            if substituted {
                return Resolution::from_candidates(
                    unique(values.iter().map(|v| normalise(Path::new(v)))),
                    fs,
                );
            }
            Resolution::from_candidates(
                unique(
                    ctx.task_search_dirs()
                        .iter()
                        .map(|b| normalise(&b.join(&r.value))),
                ),
                fs,
            )
        }

        // Relative to the importing playbook, then the project root. No role or
        // collection paths apply at play level.
        ReferenceKind::ImportPlaybook => Resolution::from_candidates(
            unique(
                [Some(ctx.file_dir.clone()), ctx.project_root.clone()]
                    .into_iter()
                    .flatten()
                    .map(|b| normalise(&b.join(&r.value))),
            ),
            fs,
        ),

        // `include_vars` searches the file's dir and `vars/`, the role `vars/`, then the
        // project root — the places Ansible looks for a vars file.
        ReferenceKind::IncludeVars => {
            let mut bases = vec![ctx.file_dir.clone(), ctx.file_dir.join("vars")];
            if let Some(role) = &ctx.role_dir {
                bases.push(role.join("vars"));
            }
            if let Some(root) = &ctx.project_root {
                bases.push(root.clone());
            }
            Resolution::from_candidates(
                unique(bases.iter().map(|b| normalise(&b.join(&r.value)))),
                fs,
            )
        }

        // The dir form runs the ported action-plugin semantics: one computed root
        // (`_set_root_dir`), then the walk with the module's own filters. Targets are the
        // files the directory loads — an editor can't open a directory — while `status`
        // stays the directory's verdict, so an empty (legal) dir still resolves.
        ReferenceKind::IncludeVarsDir => {
            let params = match &r.include_vars {
                Some(p) => (**p).clone(),
                None => include_vars::Params {
                    dir: Some(r.value.clone()),
                    ..include_vars::Params::default()
                },
            };
            let ictx = include_vars::Ctx {
                role_path: ctx.role_dir.as_deref(),
                task_dir: &ctx.file_dir,
            };
            match include_vars::load(&params, &ictx, fs) {
                include_vars::Outcome::Loaded(l) => Resolution {
                    status: Status::Resolved,
                    targets: l.files,
                    candidates: l.dir.into_iter().collect(),
                    skip_reason: None,
                },
                // Provably absent at the role path; at runtime the value decays to
                // cwd-relative, which no static verdict can cover.
                include_vars::Outcome::CwdFallback { relative } => Resolution {
                    status: Status::Missing,
                    targets: Vec::new(),
                    candidates: ctx.role_dir.iter().map(|d| d.join(&relative)).collect(),
                    skip_reason: None,
                },
                include_vars::Outcome::Failed { .. } | include_vars::Outcome::NeedsNeedle { .. } => {
                    Resolution {
                        status: Status::Missing,
                        targets: Vec::new(),
                        candidates: params
                            .dir
                            .as_deref()
                            .and_then(|d| include_vars::dir_root(d, &ictx, fs))
                            .into_iter()
                            .collect(),
                        skip_reason: None,
                    }
                }
            }
        }

        ReferenceKind::Role => match role_dir(&r.value, ctx, fs) {
            Some(dir) => {
                let res = Resolution::from_candidates(with_ext(&dir.join("tasks"), "main"), fs);
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
            let Some(role) = r.role.as_deref().and_then(|n| role_dir(n, ctx, fs)) else {
                return Resolution::skipped(SkipReason::NotInWorkspace);
            };
            Resolution::from_candidates(with_ext(&role.join("tasks"), &r.value), fs)
        }

        ReferenceKind::Module => resolve_module(&r.value, ctx, fs),
    }
}

/// Module resolution with redirect chasing — the loader's `while` loop
/// (`loader.py:740-748`) transcribed: resolve the current name; if nothing on disk but a
/// routing table renames it, follow the rename and try again. The visited set is the
/// cycle guard — a name never resolves twice, so mutually-redirecting tables terminate
/// (Ansible raises `AnsiblePluginCircularRedirect`; we quietly give up, keeping the trail).
///
/// Per name shape:
/// - **bare** (`debug:`) — implicitly `ansible.legacy.<name>`, the pre-collections search,
///   candidate order transcribed from the loader (first hit wins at both ends,
///   `loader.py:817-819,930-934`): legacy dirs, local-overrides-shipped
///   ([`FileContext::legacy_module_dirs`] documents the split), then the builtin package —
///   "package path always gets added last …" (`loader.py:497`). Not found → core's 2.10
///   split table renames it (`loader.py:956-959`).
/// - **FQCN** — `plugins/modules/` then `plugins/action/` under every collection root,
///   builtin's package dir first for `ansible.builtin.*`. Not found → the collection's own
///   `meta/runtime.yml` may rename it (how `community.general.docker_container` reaches
///   `community.docker`, a table the collection ships after moving a module out).
/// - **2 parts** — never valid (module names can't contain dots, so only 1 or 3 are
///   possible shapes); Ansible dies with "Cannot resolve … to an action or module".
///   T-042 pins the future ERROR, skipped until then.
///
/// A name that ends the chain unresolved is skipped, not warned — usually a collection
/// that isn't installed here, not a typo. The one loader step deliberately not modelled:
/// the `_<name>` deprecated-alias retry (`loader.py:940-953`) — hits are rare and Ansible
/// deprecation-warns each one itself. Deprecations/tombstones in the tables are T-064.
fn resolve_module(value: &str, ctx: &FileContext, fs: &dyn Fs) -> Resolution {
    let mut trail: Vec<PathBuf> = Vec::new();
    let mut visited: Vec<String> = vec![value.to_string()];
    let mut name = value.to_string();
    loop {
        let parts: Vec<&str> = name.split('.').collect();
        let (res, redirect) = match parts[..] {
            [bare] => {
                let file = format!("{bare}.py");
                let mut candidates: Vec<PathBuf> =
                    ctx.legacy_module_dirs().iter().map(|d| d.join(&file)).collect();
                if let Some(pkg) = &AnsibleInstall::detect().package_dir {
                    candidates.push(pkg.join("modules").join(&file));
                    candidates.push(pkg.join("plugins/action").join(&file));
                }
                let res = Resolution::from_candidates(candidates, fs);
                let redirect = (res.status != Status::Resolved)
                    .then(|| AnsibleInstall::detect().builtin_module_redirect(bare))
                    .flatten();
                (res, redirect)
            }
            [ns, coll, module] => {
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
                let res = Resolution::from_candidates(candidates, fs);
                let redirect = (res.status != Status::Resolved)
                    .then(|| collection_module_redirect(ns, coll, module, ctx, fs))
                    .flatten();
                (res, redirect)
            }
            _ => return Resolution::skipped(SkipReason::NotInWorkspace),
        };
        trail.extend(res.candidates.clone());
        if res.status == Status::Resolved {
            return Resolution { candidates: trail, ..res };
        }
        match redirect {
            Some(next) if !visited.contains(&next) => {
                visited.push(next.clone());
                name = next;
            }
            _ => {
                return Resolution {
                    status: Status::Skipped,
                    targets: Vec::new(),
                    candidates: trail,
                    skip_reason: Some(SkipReason::NotInWorkspace),
                }
            }
        }
    }
}

/// A collection's own rename for one of its module names, from the first
/// `<root>/<ns>/<coll>/meta/runtime.yml` that exists on any collection root.
fn collection_module_redirect(
    ns: &str,
    coll: &str,
    module: &str,
    ctx: &FileContext,
    fs: &dyn Fs,
) -> Option<String> {
    ctx.collection_roots()
        .iter()
        .map(|r| r.join(ns).join(coll).join("meta/runtime.yml"))
        .find(|p| fs.is_file(p))
        .and_then(|p| crate::install::module_redirect(&p, module))
}

/// Role name -> its directory. Handles plain names and 3-part FQCNs.
fn role_dir(name: &str, ctx: &FileContext, fs: &dyn Fs) -> Option<PathBuf> {
    if name.contains("{{") {
        return None;
    }
    let parts: Vec<&str> = name.split('.').collect();
    if let [ns, coll, role] = parts[..] {
        return ctx
            .collection_roots()
            .iter()
            .map(|r| r.join(ns).join(coll).join("roles").join(role))
            .find(|p| fs.is_dir(p));
    }
    ctx.roles_roots()
        .iter()
        .map(|r| r.join(name))
        .find(|p| fs.is_dir(p))
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
        let p = PathBuf::from(std::env::var("HOME").ok()?).join("app/ansible");
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

    /// Bare module names are implicitly `ansible.legacy`: a workspace `library/` shadows
    /// the builtin (demo/library/ping.py wins over ansible/modules/ping.py — "package
    /// path always gets added last", loader.py:497), plain builtins resolve into the
    /// package, and split-table names redirect to their collection home.
    #[test]
    fn bare_module_names_resolve_in_the_loaders_order() {
        let demo = PathBuf::from("../../demo").canonicalize().unwrap();
        let file = demo.join("playbook.yml");

        // Workspace library/ first — shadows the builtin even with ansible installed.
        let out = resolve_src(&file, "- ping:\n    data: x\n");
        let res = first(&out, ReferenceKind::Module);
        assert_eq!(res.status, Status::Resolved, "tried {:#?}", res.candidates);
        assert!(res.targets[0].ends_with("demo/library/ping.py"), "got {:?}", res.targets);

        if AnsibleInstall::detect().package_dir.is_none() {
            return; // no install: the remaining shapes can't resolve on this machine
        }

        // The package tree comes after every local path; the losing local candidates
        // stay in the trail, order pinned.
        let out = resolve_src(&file, "- debug:\n    msg: hi\n");
        let res = first(&out, ReferenceKind::Module);
        assert_eq!(res.status, Status::Resolved, "tried {:#?}", res.candidates);
        assert!(res.targets[0].ends_with("ansible/modules/debug.py"), "got {:?}", res.targets);
        assert!(
            res.candidates[0].ends_with("demo/library/debug.py"),
            "library dirs searched first: {:#?}",
            res.candidates
        );

        // Split-table redirect: docker_container -> community.docker.docker_container.
        // Resolved if that collection is installed; NotInWorkspace-skipped otherwise —
        // both are the loader's outcome, never Missing.
        let out = resolve_src(&file, "- docker_container:\n    name: x\n");
        let res = first(&out, ReferenceKind::Module);
        match res.status {
            Status::Resolved => assert!(
                res.targets[0]
                    .to_string_lossy()
                    .contains("community/docker/plugins/modules/docker_container.py"),
                "got {:?}",
                res.targets
            ),
            _ => assert_eq!(res.skip_reason, Some(SkipReason::NotInWorkspace)),
        }

        // 2-part names are never valid Ansible; pinned unextracted until T-042's ERROR.
        let out = resolve_src(&file, "- builtin.debug:\n    msg: hi\n");
        assert!(out.iter().all(|(r, _)| r.kind != ReferenceKind::Module));
    }

    /// Chained renames across collections' own `meta/runtime.yml` tables resolve to the
    /// final home (demo.alpha.relay → demo.beta.relay → demo.charlie.relay, a real file);
    /// a redirect cycle (loop_a ↔ loop_b) gives up quietly instead of hanging.
    #[test]
    fn chained_module_redirects_resolve_and_cycles_terminate() {
        let demo = PathBuf::from("../../demo").canonicalize().unwrap();
        let file = demo.join("playbook.yml");

        let out = resolve_src(&file, "- demo.alpha.relay:\n    x: 1\n");
        let res = first(&out, ReferenceKind::Module);
        assert_eq!(res.status, Status::Resolved, "tried {:#?}", res.candidates);
        assert!(
            res.targets[0].ends_with("demo/charlie/plugins/modules/relay.py"),
            "got {:?}",
            res.targets
        );
        // The trail keeps every hop's candidates: alpha's misses come before charlie's hit.
        assert!(
            res.candidates.iter().any(|c| crate::posix_display(c).contains("demo/alpha/plugins")),
            "first hop in the trail: {:#?}",
            res.candidates
        );

        let out = resolve_src(&file, "- demo.alpha.loop_a:\n    x: 1\n");
        let res = first(&out, ReferenceKind::Module);
        assert_eq!(res.status, Status::Skipped, "cycle must give up: {:#?}", res.candidates);
        assert_eq!(res.skip_reason, Some(SkipReason::NotInWorkspace));
    }

    #[test]
    fn resolve_with_substitutes_a_known_literal_for_navigation() {
        let d = std::env::temp_dir().join("ansible-lsp-t056");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("prod.yml"), "x: 1\n").unwrap();
        let ctx = FileContext::discover(&d.join("play.yml")); // file_dir = d
        let nodes = Document::new("- include_vars: \"{{ env }}.yml\"\n".to_string())
            .parse()
            .unwrap();
        let refs = extract(&nodes);
        let r = refs
            .iter()
            .find(|r| r.kind == ReferenceKind::IncludeVars)
            .unwrap();

        let mut lit = HashMap::new();
        lit.insert("env".to_string(), vec!["prod".to_string()]);
        let res = resolve_with(r, &ctx, &lit);
        assert_eq!(res.status, Status::Resolved);
        assert!(res.targets.iter().any(|t| t.ends_with("prod.yml")));
        // Navigation only — never a warning.
        assert_eq!(res.skip_reason, Some(SkipReason::Templated));

        // Without the literal it falls back to templated handling: no false "missing".
        let res2 = resolve_with(r, &ctx, &HashMap::new());
        assert_ne!(res2.status, Status::Missing);
    }

    #[test]
    fn substitute_literals_needs_every_token_and_a_bare_identifier() {
        let mut lit = HashMap::new();
        lit.insert("env".to_string(), vec!["prod".to_string(), "staging".to_string()]);
        // Two values -> two candidates.
        let mut got = substitute_literals("{{ env }}.yml", &lit);
        got.sort();
        assert_eq!(got, vec!["prod.yml".to_string(), "staging.yml".to_string()]);
        // A filter or unknown var -> unresolvable (empty), caller falls back.
        assert!(substitute_literals("{{ env | upper }}.yml", &lit).is_empty());
        assert!(substitute_literals("{{ other }}.yml", &lit).is_empty());
    }

    /// The four references in this repo that do NOT resolve relative to the including
    /// file. All are live, working Ansible.
    #[test]
    fn real_repo_regressions() {
        let Some(root) = repo() else { return };
        let cases = [
            (
                "roles/lustre-snapshot/tasks/query/timestamp.yml",
                "query/exists.yml",
            ),
            (
                "roles/ad/tasks/join.yml",
                "../../playbooks/tasks/select-available-node.yml",
            ),
            (
                "roles/dashboard-docker/tasks/sanity-tests/main.yml",
                "sanity-tests/database-tests.yml",
            ),
            (
                "roles/dashboard-docker/tasks/sanity-tests/main.yml",
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
        let from = root.join("roles/sync-state/tasks/http_access_point/reconcile.yml");
        let res = resolve_in(&from, "_converge_one_ap.yml");
        assert_eq!(res.status, Status::Resolved, "tried {:#?}", res.candidates);
        assert!(
            res.targets[0].ends_with("http_access_point/_converge_one_ap.yml"),
            "must reach the caller's own copy, got {:?}",
            res.targets[0]
        );
    }

    #[test]
    fn missing_file_reports_every_candidate_tried() {
        let Some(root) = repo() else { return };
        let res = resolve_in(
            &root.join("roles/sync-state/tasks/http_access_point/reconcile.yml"),
            "definitely_not_here.yml",
        );
        assert_eq!(res.status, Status::Missing);
        assert!(!res.candidates.is_empty());
    }

    /// `role_path` is the role's own directory, known at parse time. Four real
    /// references in `~/app/ansible` were unnavigable until this landed.
    #[test]
    #[ignore = "role_path expansion disabled pending chain-derived values — T-067/T-068"]
    fn role_path_expands_to_the_containing_role() {
        let Some(root) = repo() else { return };
        let res = resolve_in(
            &root.join("roles/zfs-container/tasks/main.yml"),
            "\"{{ role_path }}/../common/tasks/set-marker.yml\"",
        );
        assert_eq!(res.status, Status::Resolved, "should resolve, not glob");
        assert_eq!(
            res.targets[0],
            root.join("roles/common/tasks/set-marker.yml"),
            "`..` must collapse across the substituted role dir"
        );
    }

    /// The mistake this design exists to prevent: `playbook_dir` is a runtime fact, and
    /// substituting it with the project root alone produced 4 false "missing file"
    /// warnings, because this repo's playbooks live in `<root>/playbooks/`.
    #[test]
    fn playbook_dir_tries_every_plausible_location() {
        let Some(root) = repo() else { return };
        let res = resolve_in(
            &root.join("roles/lustre-storage/tasks/main.yml"),
            "\"{{ playbook_dir }}/../roles/common/tasks/check-prerequisites.yml\"",
        );
        assert_eq!(
            res.status,
            Status::Resolved,
            "resolves via <root>/playbooks, not <root>"
        );
    }

    /// Substituted-to-literal means fully diagnosable — the point of substituting at all.
    #[test]
    #[ignore = "role_path expansion disabled pending chain-derived values — T-067/T-068"]
    fn an_expanded_path_that_is_missing_still_warns() {
        let Some(root) = repo() else { return };
        let res = resolve_in(
            &root.join("roles/zfs-container/tasks/main.yml"),
            "\"{{ role_path }}/tasks/definitely-not-here.yml\"",
        );
        assert_eq!(res.status, Status::Missing);
        assert!(
            !res.candidates.is_empty(),
            "message must list what was tried"
        );
    }

    /// Outside a role there is no `role_path`, so it must stay unknown rather than be
    /// guessed at.
    #[test]
    fn role_path_outside_a_role_is_not_substituted() {
        let Some(root) = repo() else { return };
        let res = resolve_in(
            &root.join("site.yml"),
            "\"{{ role_path }}/tasks/whatever.yml\"",
        );
        assert_ne!(
            res.status,
            Status::Missing,
            "must not warn on an unknown value"
        );
    }

    /// Templated values may resolve to several files or none — either way they must
    /// never produce a warning.
    #[test]
    fn include_vars_dir_targets_the_loaded_files_not_the_directory() {
        let d = std::env::temp_dir().join("ansible-lsp-t017-dir");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("roles/db/tasks")).unwrap();
        std::fs::create_dir_all(d.join("roles/db/vars/prod/sub")).unwrap();
        std::fs::create_dir_all(d.join("roles/db/vars/empty")).unwrap();
        std::fs::write(d.join("roles/db/vars/prod/a.yml"), "x: 1\n").unwrap();
        std::fs::write(d.join("roles/db/vars/prod/sub/b.json"), "{\"y\": 2}\n").unwrap();
        std::fs::write(d.join("roles/db/vars/prod/notes.txt"), "").unwrap();
        let file = d.join("roles/db/tasks/main.yml");
        std::fs::write(&file, "").unwrap();

        // Unprefixed value resolves under the role's vars/; targets are the files the
        // walk loads (recursive, json included), never the directory itself. The .txt
        // would fail the task, and ignore_unknown_extensions=true skips it instead.
        let out = resolve_src(&file, "- include_vars: { dir: prod, ignore_unknown_extensions: true }\n");
        let res = first(&out, ReferenceKind::IncludeVarsDir);
        assert_eq!(res.status, Status::Resolved);
        assert_eq!(
            res.targets,
            vec![
                d.join("roles/db/vars/prod/a.yml"),
                d.join("roles/db/vars/prod/sub/b.json"),
            ]
        );

        // An empty directory is legal and resolves — with nothing to navigate to.
        let res = resolve_src(&file, "- include_vars: { dir: empty }\n");
        let res = first(&res, ReferenceKind::IncludeVarsDir);
        assert_eq!(res.status, Status::Resolved);
        assert!(res.targets.is_empty());

        // In-role `vars/`-prefixed miss: provably absent at the role path; runtime decays
        // to cwd. Missing, with the one role-relative candidate named.
        let res = resolve_src(&file, "- include_vars: { dir: vars/nope }\n");
        let res = first(&res, ReferenceKind::IncludeVarsDir);
        assert_eq!(res.status, Status::Missing);
        assert_eq!(res.candidates, vec![d.join("roles/db/vars/nope")]);

        // Templated dir: unknowable, skipped, never warned.
        let res = resolve_src(&file, "- include_vars: { dir: \"{{ env }}\" }\n");
        let res = first(&res, ReferenceKind::IncludeVarsDir);
        assert_eq!(res.status, Status::Skipped);
    }

    #[test]
    fn include_vars_dir_outside_a_role_uses_the_task_files_dir() {
        let d = std::env::temp_dir().join("ansible-lsp-t017-dir-norole");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("playbooks/setup/settings")).unwrap();
        std::fs::write(d.join("playbooks/setup/settings/c.yml"), "z: 3\n").unwrap();
        let file = d.join("playbooks/setup/tasks.yml");
        std::fs::write(&file, "").unwrap();

        let out = resolve_src(&file, "- include_vars: { dir: settings }\n");
        let res = first(&out, ReferenceKind::IncludeVarsDir);
        assert_eq!(res.status, Status::Resolved);
        assert_eq!(res.targets, vec![d.join("playbooks/setup/settings/c.yml")]);
    }

    /// `inventory_dir` used to borrow the playbook-dir guesses; it is per-host and set by
    /// `-i` at launch, so it must stay templated — glob, never substitute, never warn.
    #[test]
    fn inventory_dir_is_not_substituted() {
        let d = std::env::temp_dir().join("ansible-lsp-t070-invdir");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("tasks")).unwrap();
        std::fs::write(d.join("ansible.cfg"), "[defaults]\n").unwrap();
        // Planted exactly where the old project-root guess would have hit.
        std::fs::write(d.join("only_here.yml"), "").unwrap();
        let file = d.join("tasks/main.yml");
        std::fs::write(&file, "").unwrap();

        // Substitution would resolve this as a complete path with no skip reason; the
        // templated route is only reachable when the `{{ }}` survives expansion.
        let out = resolve_src(&file, "- include_tasks: \"{{ inventory_dir }}/only_here.yml\"\n");
        let res = first(&out, ReferenceKind::IncludeTasks);
        assert_eq!(res.skip_reason, Some(SkipReason::Templated));

        // And an absent target skips rather than warns.
        let out = resolve_src(&file, "- include_tasks: \"{{ inventory_dir }}/absent.yml\"\n");
        let res = first(&out, ReferenceKind::IncludeTasks);
        assert_eq!(res.status, Status::Skipped);
    }

    #[test]
    fn templated_paths_never_warn() {
        let Some(root) = repo() else { return };
        for value in [
            "\"{{ ap_protocol }}/validate.yml\"",
            "\"{{ nothing_matches_this }}/xyzzy.yml\"",
        ] {
            let res = resolve_in(
                &root.join("roles/sync-state/tasks/http_access_point/reconcile.yml"),
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
        assert!(
            res.targets.len() >= 2,
            "expected several, got {:?}",
            res.targets
        );
    }

    #[test]
    fn import_playbook_resolves_relative_to_the_importer() {
        let Some(root) = repo() else { return };
        for (from, target) in [
            ("playbooks/lustre-full-deploy.yml", "lustre-infrastructure.yml"),
            ("playbooks/app/setup.yml", "../../network-setup.yml"),
        ] {
            let out = resolve_src(&root.join(from), &format!("- import_playbook: {target}\n"));
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
        assert_eq!(
            first(&out, ReferenceKind::TasksFrom).status,
            Status::Resolved
        );
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
    /// break in the repo: site.yml lists `lustre`, and roles/lustre does not exist.
    #[test]
    fn unknown_role_name_is_reported() {
        let Some(root) = repo() else { return };
        let out = resolve_src(
            &root.join("site.yml"),
            "- hosts: all\n  roles:\n    - lustre\n",
        );
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
            "- community.lvm.pool_create:\n    name: p\n",
        );
        let res = first(&out, ReferenceKind::Module);
        assert_eq!(res.status, Status::Resolved, "tried {:#?}", res.candidates);
        assert!(res.targets[0].ends_with("community/lvm/plugins/modules/pool_create.py"));
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
        let Ok(home) = std::env::var("HOME") else {
            return;
        };
        let root = PathBuf::from(&home).join("app/ansible");
        if !root.is_dir() {
            return;
        }
        // find largest yml
        let mut biggest: Option<(PathBuf, u64)> = None;
        fn walk(d: &Path, best: &mut Option<(PathBuf, u64)>) {
            let Ok(rd) = std::fs::read_dir(d) else { return };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    let n = p
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    if n == ".git" || n == "__pycache__" {
                        continue;
                    }
                    walk(&p, best);
                } else if p.extension().map_or(false, |x| x == "yml") {
                    let s = e.metadata().map(|m| m.len()).unwrap_or(0);
                    if best.as_ref().map_or(true, |(_, b)| s > *b) {
                        *best = Some((p, s));
                    }
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
        for r in &refs {
            let _ = resolve(r, &ctx);
        }
        println!("resolve all:  {:?}", t.elapsed());

        let by_kind = |k: ReferenceKind| refs.iter().filter(|r| r.kind == k).count();
        println!(
            "  modules={} roles={} tasks={}",
            by_kind(ReferenceKind::Module),
            by_kind(ReferenceKind::Role),
            by_kind(ReferenceKind::IncludeTasks)
        );
    }
}
