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
    /// A missing alternative in a first-match `vars_files` list — absence is the
    /// construct working as designed, so the group reference owns the verdict.
    GroupAlternative,
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
        // Deliberately the same id as a single entry: a first-match group that resolves
        // nothing is still "the file this play claims to load doesn't exist".
        (ReferenceKind::VarsFiles, _) => "missing-file",
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
    // A group's value is the joined alternatives list, not a path — substitution would
    // build nonsense candidates from it. The group resolver handles templating itself.
    if r.value.contains("{{") && r.vars_files_group.is_none() {
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
        ReferenceKind::VarsFiles => {
            Some(vec![ctx.file_dir.join("vars"), ctx.file_dir.clone()])
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
    // A first-match `vars_files` list resolves as a unit: first alternative that exists
    // wins, and only the group — never a member — can be Missing.
    if let Some(alts) = &r.vars_files_group {
        return resolve_vars_files_group(alts, ctx, fs);
    }

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
            ReferenceKind::VarsFiles => vec![ctx.file_dir.join("vars"), ctx.file_dir.clone()],
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

        ReferenceKind::VarsFiles => {
            let res = if substituted {
                // An expansion is a complete path — the vars/ prepend does not apply.
                Resolution::from_candidates(
                    unique(values.iter().map(|v| normalise(Path::new(v)))),
                    fs,
                )
            } else {
                Resolution::from_candidates(vars_files_candidates(&r.value, &ctx.file_dir), fs)
            };
            match (res.status, r.grouped) {
                (Status::Missing, true) => Resolution {
                    status: Status::Skipped,
                    skip_reason: Some(SkipReason::GroupAlternative),
                    ..res
                },
                _ => res,
            }
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
                let probe = RoleExts::default().candidates(&dir.join("tasks"), "main", false);
                let res = Resolution::from_candidates(probe, fs);
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
            let probe = RoleExts::default().candidates(&role.join("tasks"), &r.value, true);
            Resolution::from_candidates(probe, fs)
        }

        ReferenceKind::Module => resolve_module(&r.value, ctx, fs),
    }
}

/// `MODULE_IGNORE_EXTS` (`constants.py:62` REJECT_EXTS + `.yaml/.yml/.ini`,
/// `base.yml:1799-1801`): suffixes both module finders refuse even on a basename match —
/// compiled artifacts, backups, docs.
const MODULE_IGNORE_EXTS: &[&str] =
    &[".pyc", ".pyo", ".swp", ".bak", "~", ".rpm", ".md", ".txt", ".rst", ".yaml", ".yml", ".ini"];

/// The files in `dir` Ansible would accept as module `name`: modules can be any
/// executable, so `name` bare or with any one extension counts (T-093). Sorted, first
/// wins: exactly the FQCN finder (`loader.py:704-719`); the legacy finder takes
/// `os.listdir` order, which is filesystem-arbitrary, so sorted stands in as the
/// deterministic pick there too.
fn module_files_named(dir: &Path, name: &str, fs: &dyn Fs) -> Vec<PathBuf> {
    let mut hits: Vec<PathBuf> = fs
        .read_dir(dir)
        .into_iter()
        .filter(|(p, kind)| *kind == crate::fs::Kind::File && is_module_named(p, name))
        .map(|(p, _)| p)
        .collect();
    hits.sort();
    hits
}

/// `splitext(file) == name` (`loader.py:907-908`) — the whole filename, or everything
/// before its last dot — minus the ignore list.
fn is_module_named(p: &Path, name: &str) -> bool {
    let Some(f) = p.file_name().and_then(|f| f.to_str()) else { return false };
    let stem = f.rsplit_once('.').map_or(f, |(s, _)| s);
    stem == name && !MODULE_IGNORE_EXTS.iter().any(|ext| f.ends_with(ext))
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
///
/// In `plugins/modules/` and the legacy dirs a module matches with ANY extension or none —
/// modules are executables shipped to the target, not controller classes, so their loader
/// has no `.py` suffix (`loader.py:786-788`; T-093). `plugins/action/` stays `.py`-only.
fn resolve_module(value: &str, ctx: &FileContext, fs: &dyn Fs) -> Resolution {
    let mut trail: Vec<PathBuf> = Vec::new();
    let mut visited: Vec<String> = vec![value.to_string()];
    let mut name = value.to_string();
    loop {
        let parts: Vec<&str> = name.split('.').collect();
        let (res, redirect) = match parts[..] {
            [bare] => {
                let mut candidates: Vec<PathBuf> = Vec::new();
                for dir in ctx.legacy_module_dirs() {
                    let found = module_files_named(&dir, bare, fs);
                    if found.is_empty() {
                        // Keep the dir in the trail so diagnostics still name it.
                        candidates.push(dir.join(format!("{bare}.py")));
                    } else {
                        candidates.extend(found);
                    }
                }
                if let Some(pkg) = &AnsibleInstall::detect().package_dir {
                    let file = format!("{bare}.py");
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
                let mut candidates: Vec<PathBuf> = Vec::new();
                for root in ctx.collection_roots() {
                    let base = root.join(ns).join(coll).join("plugins");
                    let found = module_files_named(&base.join("modules"), module, fs);
                    if found.is_empty() {
                        candidates.push(base.join("modules").join(format!("{module}.py")));
                    } else {
                        candidates.extend(found);
                    }
                    // Action plugins are controller-side Python classes, so their loader
                    // hard-requires `.py` (`loader.py:782-784`) — no glob here.
                    candidates.push(base.join("action").join(format!("{module}.py")));
                }
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
/// Where a `vars_files:` entry can land — ansible-core's
/// `path_dwim_relative_stack(play.get_search_path(), 'vars', entry)` (`dataloader.py:345-390`,
/// live-verified on 2.21.2) with the loader basedir statically equal to the play's own dir:
/// `<play dir>/vars/<entry>` — skipped when the entry's first component is literally `vars`,
/// so `vars/x.yml` never doubles into `vars/vars/` — then `<play dir>/<entry>`. An absolute
/// (or `~`) entry is a single candidate with no `vars/` prepend. No role `vars/`, no project
/// root, and no extension guessing (`foo` does not find `foo.yml`). Lexically normalised so
/// `../` collapses before any existence check.
///
/// A miss is still only ever a warning: ansible-core 2.21.2 *silently skips* a missing
/// `vars_files` file (its "we raise an error" comment is stale) — the play runs, the
/// variables are just never set.
/// First-found over a `vars_files` alternatives list. Resolves via the first alternative
/// that exists; Missing only when none can. Any templated alternative makes the group
/// undecidable — it might resolve at runtime — so the group defers rather than warns.
fn resolve_vars_files_group(alts: &[String], ctx: &FileContext, fs: &dyn Fs) -> Resolution {
    let mut tried = Vec::new();
    let mut templated = false;
    for alt in alts {
        if alt.contains("{{") {
            templated = true;
            continue;
        }
        let cands = vars_files_candidates(alt, &ctx.file_dir);
        let hit = cands.iter().find(|p| fs.is_file(p)).cloned();
        tried.extend(cands);
        if let Some(p) = hit {
            return Resolution {
                status: Status::Resolved,
                targets: vec![p],
                candidates: unique(tried.into_iter()),
                skip_reason: None,
            };
        }
    }
    if templated {
        return Resolution::skipped(SkipReason::Templated);
    }
    Resolution {
        status: Status::Missing,
        targets: Vec::new(),
        candidates: unique(tried.into_iter()),
        skip_reason: None,
    }
}

pub fn vars_files_candidates(entry: &str, file_dir: &Path) -> Vec<PathBuf> {
    if let Some(rest) = entry.strip_prefix("~/") {
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
        return match home {
            Some(h) => vec![normalise(&Path::new(&h).join(rest))],
            None => vec![PathBuf::from(entry)],
        };
    }
    if Path::new(entry).is_absolute() {
        return vec![normalise(Path::new(entry))];
    }
    let mut out = Vec::new();
    if entry.split('/').next() != Some("vars") {
        out.push(normalise(&file_dir.join("vars").join(entry)));
    }
    out.push(normalise(&file_dir.join(entry)));
    out
}

/// The extensions a role file may carry, in probe order.
///
/// A value rather than a constant so a test can hold the *wrong* list next to the right
/// one — pinning "`.json` is in here" from both sides, instead of by hand-reverting the
/// code and trusting the reverter. [`Default`] is the only list production ever uses:
/// hardcoded in `Role._load_role_yaml` (`role/__init__.py:421-422`) and deliberately
/// *not* `C.YAML_FILENAME_EXTENSIONS`, "to maintain portability" — so a plugin adding an
/// extension elsewhere cannot change what a role loads. There is no config key for this
/// and there should be no way to reach one from here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RoleExts(&'static [&'static str]);

impl Default for RoleExts {
    fn default() -> Self {
        Self(&[".yml", ".yaml", ".json"])
    }
}

impl RoleExts {
    /// The role files `stem` could name in `dir`, in the order Ansible probes them.
    ///
    /// The extensionless form is always a candidate; which end it sits on is the whole
    /// question, because `find_vars_files` breaks on the first hit
    /// (`dataloader.py:483-491`) and never merges — every later candidate is dead, not
    /// shadowed. The default entry point appends `''` **last**, so `main.yml` beats a bare
    /// `main`; any `*_from:` inserts it **first** (`role/__init__.py:426-431`), so the
    /// literal name given wins over `<name>.yml`. Hence `bare_first`, set from whether the
    /// reference carried a `*_from`.
    ///
    /// A stem that already ends in an extension gets the suffixes appended anyway
    /// (`setup.yml.json`) — Ansible builds the same nonsense candidates, and with `''`
    /// first the literal always wins before they are reached.
    ///
    /// A directory is not a hit here: roles pass `allow_dir=False`, which skips a matching
    /// directory and keeps probing (`dataloader.py:484-488`), and [`Fs::is_file`] agrees.
    ///
    /// One allocation per candidate and one for the vec: the suffix is appended into the
    /// `PathBuf`'s own buffer, which is sized up front, so there is no throwaway
    /// `format!` string and no realloc on the way.
    fn candidates(self, dir: &Path, stem: &str, bare_first: bool) -> Vec<PathBuf> {
        let joined = |ext: &str| {
            let mut p = PathBuf::with_capacity(dir.as_os_str().len() + 1 + stem.len() + ext.len());
            p.push(dir);
            p.push(stem);
            // Straight onto the filename — `push` would insert a separator.
            p.as_mut_os_string().push(ext);
            p
        };
        let mut out = Vec::with_capacity(self.0.len() + 1);
        if bare_first {
            out.push(joined(""));
        }
        out.extend(self.0.iter().map(|e| joined(e)));
        if !bare_first {
            out.push(joined(""));
        }
        out
    }
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

    fn t016_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ansible-lsp-t016-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("vars")).unwrap();
        d
    }

    #[test]
    fn handler_includes_anchor_at_handlers_not_tasks() {
        // T-092: an include written in a role's handlers/ loads from handlers/ —
        // `'handlers' if isinstance(original_task, Handler) else 'tasks'`
        // (included_file.py:172) — with the role's tasks/ as a later legal fallback
        // (dataloader.py:311-313), not the primary base.
        let d = t016_dir("handlers");
        std::fs::create_dir_all(d.join("roles/r/handlers")).unwrap();
        std::fs::create_dir_all(d.join("roles/r/tasks")).unwrap();
        // The same basename in both dirs pins which one wins.
        std::fs::write(d.join("roles/r/handlers/restart.yml"), "").unwrap();
        std::fs::write(d.join("roles/r/tasks/restart.yml"), "").unwrap();
        // Only in tasks/: reachable from a handler include via the fallback.
        std::fs::write(d.join("roles/r/tasks/shared.yml"), "").unwrap();
        let handler_file = d.join("roles/r/handlers/main.yml");
        std::fs::write(&handler_file, "").unwrap();

        let out = resolve_src(&handler_file, "- include_tasks: restart.yml\n");
        let res = first(&out, ReferenceKind::IncludeTasks);
        assert_eq!(res.status, Status::Resolved);
        assert_eq!(res.targets, vec![d.join("roles/r/handlers/restart.yml")]);

        let out = resolve_src(&handler_file, "- include_tasks: shared.yml\n");
        let res = first(&out, ReferenceKind::IncludeTasks);
        assert_eq!(res.status, Status::Resolved, "tried {:#?}", res.candidates);
        assert_eq!(res.targets, vec![d.join("roles/r/tasks/shared.yml")]);

        // Includes written under tasks/ still anchor at tasks/.
        let task_file = d.join("roles/r/tasks/main.yml");
        std::fs::write(&task_file, "").unwrap();
        let out = resolve_src(&task_file, "- include_tasks: restart.yml\n");
        let res = first(&out, ReferenceKind::IncludeTasks);
        assert_eq!(res.targets, vec![d.join("roles/r/tasks/restart.yml")]);
    }

    /// The demo fixture for T-092 keeps its promise: the files its comments point at
    /// exist and win. Same scenario as `handler_includes_anchor_at_handlers_not_tasks`,
    /// but against the checked-in demo tree — a hand-made fixture can silently lack the
    /// file its comments describe (this one shipped without handlers/restart.yml).
    #[test]
    fn demo_notifier_handler_includes_resolve_as_documented() {
        let file = Path::new("../../demo/roles/notifier/handlers/main.yml")
            .canonicalize()
            .unwrap();

        let out = resolve_src(&file, "- include_tasks: restart.yml\n");
        let res = first(&out, ReferenceKind::IncludeTasks);
        assert_eq!(res.status, Status::Resolved);
        assert!(
            res.targets[0].ends_with("notifier/handlers/restart.yml"),
            "handlers/ must win over the tasks/ decoy: {:?}",
            res.targets
        );

        let out = resolve_src(&file, "- include_tasks: shared.yml\n");
        let res = first(&out, ReferenceKind::IncludeTasks);
        assert_eq!(res.status, Status::Resolved, "tried {:#?}", res.candidates);
        assert!(
            res.targets[0].ends_with("notifier/tasks/shared.yml"),
            "the tasks/ fallback resolves shared.yml: {:?}",
            res.targets
        );
    }

    #[test]
    fn vars_files_candidate_order_vars_subdir_wins() {
        let d = t016_dir("order");
        std::fs::write(d.join("x.yml"), "a: 1\n").unwrap();
        std::fs::write(d.join("vars/x.yml"), "a: 2\n").unwrap();
        let file = d.join("site.yml");
        std::fs::write(&file, "").unwrap();

        let out = resolve_src(&file, "- hosts: all\n  vars_files: [x.yml]\n");
        let res = first(&out, ReferenceKind::VarsFiles);
        assert_eq!(res.status, Status::Resolved);
        // `<dir>/vars/` is probed before the dir itself — the order Ansible loads in.
        assert_eq!(res.targets, vec![d.join("vars/x.yml")]);
        assert_eq!(res.candidates, vec![d.join("vars/x.yml"), d.join("x.yml")]);
    }

    #[test]
    fn vars_files_vars_prefixed_entry_skips_the_prepend() {
        let d = t016_dir("guard");
        std::fs::write(d.join("vars/x.yml"), "a: 1\n").unwrap();
        let file = d.join("site.yml");
        std::fs::write(&file, "").unwrap();

        // `vars/…` never doubles into `vars/vars/…`.
        let out = resolve_src(&file, "- hosts: all\n  vars_files: [vars/x.yml]\n");
        let res = first(&out, ReferenceKind::VarsFiles);
        assert_eq!(res.status, Status::Resolved);
        assert_eq!(res.candidates, vec![d.join("vars/x.yml")]);

        // The guard is upstream's literal string check — `./vars/…` does not trigger it.
        let out = resolve_src(&file, "- hosts: all\n  vars_files: [./vars/x.yml]\n");
        let res = first(&out, ReferenceKind::VarsFiles);
        assert_eq!(res.status, Status::Resolved);
        assert_eq!(
            res.candidates,
            vec![d.join("vars/vars/x.yml"), d.join("vars/x.yml")]
        );
    }

    #[test]
    fn vars_files_parent_paths_normalise_before_the_check() {
        let d = t016_dir("parent");
        std::fs::create_dir_all(d.join("plays")).unwrap();
        std::fs::create_dir_all(d.join("shared")).unwrap();
        std::fs::write(d.join("shared/x.yml"), "a: 1\n").unwrap();
        let file = d.join("plays/site.yml");
        std::fs::write(&file, "").unwrap();

        let out = resolve_src(&file, "- hosts: all\n  vars_files: [../shared/x.yml]\n");
        let res = first(&out, ReferenceKind::VarsFiles);
        assert_eq!(res.status, Status::Resolved);
        assert_eq!(res.targets, vec![d.join("shared/x.yml")]);
        for c in &res.candidates {
            assert!(
                !c.components().any(|p| p == std::path::Component::ParentDir),
                "candidates are normalised before the check: {c:?}"
            );
        }
    }

    #[test]
    fn vars_files_absolute_entry_is_one_candidate() {
        let d = t016_dir("abs");
        let target = d.join("abs.yml");
        std::fs::write(&target, "a: 1\n").unwrap();
        let file = d.join("site.yml");
        std::fs::write(&file, "").unwrap();

        let src = format!(
            "- hosts: all\n  vars_files: ['{}']\n",
            target.to_string_lossy()
        );
        let out = resolve_src(&file, &src);
        let res = first(&out, ReferenceKind::VarsFiles);
        assert_eq!(res.status, Status::Resolved);
        // No `vars/` prepend for an absolute entry — exactly one candidate.
        assert_eq!(res.candidates, vec![target.clone()]);
        assert_eq!(res.targets, vec![target]);
    }

    #[test]
    fn vars_files_no_role_vars_or_project_root_fallback() {
        let d = t016_dir("bases");
        std::fs::create_dir_all(d.join("playbooks")).unwrap();
        // Planted where include_vars would look, but vars_files must not: project root.
        std::fs::write(d.join("only-at-root.yml"), "a: 1\n").unwrap();
        std::fs::write(d.join("ansible.cfg"), "").unwrap();
        let file = d.join("playbooks/site.yml");
        std::fs::write(&file, "").unwrap();

        let out = resolve_src(&file, "- hosts: all\n  vars_files: [only-at-root.yml]\n");
        let res = first(&out, ReferenceKind::VarsFiles);
        assert_eq!(res.status, Status::Missing);
        assert_eq!(
            res.candidates,
            vec![
                d.join("playbooks/vars/only-at-root.yml"),
                d.join("playbooks/only-at-root.yml"),
            ]
        );
    }

    #[test]
    fn vars_files_group_first_found_wins_and_all_missing_is_one_missing() {
        let d = t016_dir("group");
        std::fs::write(d.join("vars/b.yml"), "a: 1\n").unwrap();
        std::fs::write(d.join("vars/c.yml"), "a: 2\n").unwrap();
        let file = d.join("site.yml");
        std::fs::write(&file, "").unwrap();

        // First existing alternative wins, even with a later one also present.
        let out = resolve_src(
            &file,
            "- hosts: all\n  vars_files:\n    - - vars/a.yml\n      - vars/b.yml\n      - vars/c.yml\n",
        );
        let group = &out.iter().find(|(r, _)| r.vars_files_group.is_some()).unwrap().1;
        assert_eq!(group.status, Status::Resolved);
        assert_eq!(group.targets, vec![d.join("vars/b.yml")]);
        // The missing alternative is the construct working as designed — never Missing.
        let (_, a_res) = out.iter().find(|(r, _)| r.value == "vars/a.yml").unwrap();
        assert_eq!(a_res.status, Status::Skipped);
        assert_eq!(a_res.skip_reason, Some(SkipReason::GroupAlternative));

        // None exists: exactly the group is Missing, naming every candidate tried.
        let out = resolve_src(
            &file,
            "- hosts: all\n  vars_files:\n    - - vars/nope-a.yml\n      - vars/nope-b.yml\n",
        );
        let missing: Vec<_> = out.iter().filter(|(_, res)| res.status == Status::Missing).collect();
        assert_eq!(missing.len(), 1);
        assert!(missing[0].0.vars_files_group.is_some());
        assert_eq!(
            missing[0].1.candidates,
            vec![d.join("vars/nope-a.yml"), d.join("vars/nope-b.yml")]
        );
    }

    #[test]
    fn vars_files_group_with_templated_alternative_never_warns() {
        let d = t016_dir("group-tmpl");
        let file = d.join("site.yml");
        std::fs::write(&file, "").unwrap();

        let out = resolve_src(
            &file,
            "- hosts: all\n  vars_files:\n    - - \"{{ env }}.yml\"\n      - vars/nope.yml\n",
        );
        let group = &out.iter().find(|(r, _)| r.vars_files_group.is_some()).unwrap().1;
        assert_eq!(group.status, Status::Skipped);
        assert_eq!(group.skip_reason, Some(SkipReason::Templated));
        assert!(out.iter().all(|(_, res)| res.status != Status::Missing));
    }

    #[test]
    fn vars_files_templated_entry_globs_for_navigation() {
        let d = t016_dir("glob");
        std::fs::write(d.join("vars/prod.yml"), "a: 1\n").unwrap();
        std::fs::write(d.join("vars/staging.yml"), "a: 2\n").unwrap();
        let file = d.join("site.yml");
        std::fs::write(&file, "").unwrap();

        let out = resolve_src(&file, "- hosts: all\n  vars_files: [\"vars/{{ env }}.yml\"]\n");
        let res = first(&out, ReferenceKind::VarsFiles);
        assert_eq!(res.status, Status::Resolved);
        assert_eq!(res.skip_reason, Some(SkipReason::Templated), "offer, never assert");
        assert!(res.targets.contains(&d.join("vars/prod.yml")));
        assert!(res.targets.contains(&d.join("vars/staging.yml")));
    }

    #[test]
    fn resolve_with_substitutes_known_literals_for_vars_files() {
        let d = t016_dir("subst");
        std::fs::write(d.join("vars/prod.yml"), "a: 1\n").unwrap();
        let file = d.join("site.yml");
        std::fs::write(&file, "").unwrap();

        let doc = Document::new("- hosts: all\n  vars_files: [\"vars/{{ env }}.yml\"]\n".to_string());
        let ctx = FileContext::discover(&file);
        let r = extract(&doc.parse().unwrap())
            .into_iter()
            .find(|r| r.kind == ReferenceKind::VarsFiles)
            .unwrap();
        let literals = HashMap::from([("env".to_string(), vec!["prod".to_string()])]);
        let res = resolve_with(&r, &ctx, &literals);
        assert_eq!(res.status, Status::Resolved);
        assert_eq!(res.skip_reason, Some(SkipReason::Templated), "navigation only");
        assert_eq!(res.targets, vec![d.join("vars/prod.yml")]);
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
                crate::posix_display(&res.targets[0])
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

    /// Modules are any executable: the legacy finder caches every library/ file by its
    /// splitext base name (`loader.py:899-927`), so `.ps1`, `.sh` and extensionless all
    /// run under Ansible — only `.py` resolved before T-093.
    #[test]
    fn legacy_modules_match_any_extension() {
        let d = t016_dir("modext");
        std::fs::create_dir_all(d.join("library")).unwrap();
        let file = d.join("site.yml");
        std::fs::write(&file, "").unwrap();

        std::fs::write(d.join("library/winmod.ps1"), "# powershell\n").unwrap();
        let out = resolve_src(&file, "- winmod:\n    path: x\n");
        let res = first(&out, ReferenceKind::Module);
        assert_eq!(res.status, Status::Resolved, "tried {:#?}", res.candidates);
        assert_eq!(res.targets, vec![d.join("library/winmod.ps1")]);

        std::fs::write(d.join("library/rawmod"), "#!/bin/sh\n").unwrap();
        let out = resolve_src(&file, "- rawmod:\n    path: x\n");
        let res = first(&out, ReferenceKind::Module);
        assert_eq!(res.status, Status::Resolved, "tried {:#?}", res.candidates);
        assert_eq!(res.targets, vec![d.join("library/rawmod")]);

        // MODULE_IGNORE_EXTS: a doc file sharing the base name is never the module.
        std::fs::write(d.join("library/notes.md"), "").unwrap();
        let out = resolve_src(&file, "- notes:\n    path: x\n");
        let res = first(&out, ReferenceKind::Module);
        assert_ne!(res.status, Status::Resolved, "got {:#?}", res.targets);
    }

    /// The ambiguous case pinned: several files share the base name, sorted order picks
    /// the first — the extensionless one, "shortest match first" (`loader.py:712`).
    #[test]
    fn ambiguous_module_match_takes_sorted_first() {
        let d = t016_dir("modambig");
        std::fs::create_dir_all(d.join("library")).unwrap();
        let file = d.join("site.yml");
        std::fs::write(&file, "").unwrap();
        std::fs::write(d.join("library/both"), "#!/bin/sh\n").unwrap();
        std::fs::write(d.join("library/both.ps1"), "").unwrap();
        std::fs::write(d.join("library/both.py"), "").unwrap();

        let out = resolve_src(&file, "- both:\n    path: x\n");
        let res = first(&out, ReferenceKind::Module);
        assert_eq!(res.status, Status::Resolved, "tried {:#?}", res.candidates);
        assert_eq!(res.targets, vec![d.join("library/both")]);
        // The losers stay in the trail.
        assert!(res.candidates.contains(&d.join("library/both.ps1")), "{:#?}", res.candidates);
        assert!(res.candidates.contains(&d.join("library/both.py")), "{:#?}", res.candidates);
    }

    /// The demo tree's non-Python modules (T-093): bash in `library/` and in a
    /// collection's `plugins/modules/` — pinned against the checked-in fixtures so the
    /// demo can't go stale. The FQCN finder fuzzy-matches extensions exactly like the
    /// legacy one (`loader.py:704-719`); only action plugins are `.py`-bound.
    #[test]
    fn demo_non_python_modules_resolve() {
        let demo = PathBuf::from("../../demo").canonicalize().unwrap();
        let file = demo.join("playbook.yml");

        let out = resolve_src(&file, "- sweep:\n    paths: /var/tmp\n");
        let res = first(&out, ReferenceKind::Module);
        assert_eq!(res.status, Status::Resolved, "tried {:#?}", res.candidates);
        assert!(res.targets[0].ends_with("demo/library/sweep.sh"), "got {:?}", res.targets);

        let out = resolve_src(&file, "- demo.charlie.pulse:\n    interval: 5\n");
        let res = first(&out, ReferenceKind::Module);
        assert_eq!(res.status, Status::Resolved, "tried {:#?}", res.candidates);
        assert!(
            res.targets[0].ends_with("demo/charlie/plugins/modules/pulse.sh"),
            "got {:?}",
            res.targets
        );
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

    /// The T-091 demo keeps its promises: every GOOD line in
    /// `demo/tasks/role_entrypoints.yml` lands on the file its comment names, and the one
    /// BAD line misses. A hand-made fixture can silently lack the file it describes —
    /// T-092's shipped without `handlers/restart.yml` — so the demo is asserted, not
    /// trusted.
    #[test]
    fn demo_role_entry_points_resolve_as_documented() {
        let file = Path::new("../../demo/tasks/role_entrypoints.yml")
            .canonicalize()
            .unwrap();
        let src = std::fs::read_to_string(&file).unwrap();
        let out = resolve_src(&file, &src);
        let from: Vec<&Resolution> = out
            .iter()
            .filter(|(r, _)| r.kind == ReferenceKind::TasksFrom)
            .map(|(_, res)| res)
            .collect();

        // The default entry point: main.yml wins, main.yaml is dead.
        let role = first(&out, ReferenceKind::Role);
        assert_eq!(role.status, Status::Resolved, "tried {:#?}", role.candidates);
        assert!(
            role.targets[0].ends_with("entrypoints/tasks/main.yml"),
            "main.yml must win over main.yaml: {:?}",
            role.targets
        );

        // In file order: report -> .json, setup -> extensionless, setup.yml -> the
        // shadowed file, legacy -> nothing, legacy.jamil -> the file itself.
        let want = [
            Some("entrypoints/tasks/report.json"),
            Some("entrypoints/tasks/setup"),
            Some("entrypoints/tasks/setup.yml"),
            None,
            Some("entrypoints/tasks/legacy.jamil"),
        ];
        assert_eq!(from.len(), want.len(), "demo lost a tasks_from example");
        for (res, want) in from.iter().zip(want) {
            match want {
                Some(end) => {
                    assert_eq!(res.status, Status::Resolved, "tried {:#?}", res.candidates);
                    assert!(res.targets[0].ends_with(end), "want {end}, got {:?}", res.targets);
                }
                // `.jamil` is not a role extension, so the bare name reaches nothing.
                None => assert_eq!(res.status, Status::Missing, "got {:?}", res.targets),
            }
        }
    }

    /// T-091: role files carry `.yml`, `.yaml`, `.json` or no extension at all, and
    /// which end of that list the extensionless form sits on flips with `*_from`
    /// (`role/__init__.py:421-431`). One tree pins both ends, and pins that the loser
    /// of a shadowing pair is dead rather than also loaded — `find_vars_files` breaks
    /// on the first hit.
    #[test]
    fn role_file_extension_order_flips_with_tasks_from() {
        let d = t016_dir("role-exts");
        let tasks = d.join("roles/r/tasks");
        std::fs::create_dir_all(&tasks).unwrap();
        for f in ["main.yml", "main.yaml", "setup", "setup.yml", "data.json"] {
            std::fs::write(tasks.join(f), "").unwrap();
        }
        std::fs::create_dir_all(d.join("roles/bare/tasks")).unwrap();
        std::fs::write(d.join("roles/bare/tasks/main"), "").unwrap();
        let play = d.join("site.yml");
        std::fs::write(&play, "").unwrap();

        // Default entry point: `''` last, so main.yml wins and main.yaml is unreachable.
        let out = resolve_src(&play, "- include_role:\n    name: r\n");
        let res = first(&out, ReferenceKind::Role);
        assert_eq!(res.targets, vec![tasks.join("main.yml")]);
        assert_eq!(
            res.candidates,
            ["main.yml", "main.yaml", "main.json", "main"].map(|f| tasks.join(f))
        );

        // With a tasks_from: `''` first, so the literal name beats setup.yml.
        let out = resolve_src(&play, "- include_role: { name: r, tasks_from: setup }\n");
        let res = first(&out, ReferenceKind::TasksFrom);
        assert_eq!(res.targets, vec![tasks.join("setup")]);
        assert_eq!(
            res.candidates,
            ["setup", "setup.yml", "setup.yaml", "setup.json"].map(|f| tasks.join(f))
        );

        // Both axes driven directly, so the pre-T-091 behaviour stays pinned in the
        // suite instead of living in whoever remembers to revert the code. The old list
        // is the only difference; the fixture and the probe are the same ones above.
        let old = RoleExts(&[".yml", ".yaml"]);
        let hit = |exts: RoleExts, stem: &str, bare_first: bool| {
            exts.candidates(&tasks, stem, bare_first)
                .into_iter()
                .find(|p| p.is_file())
        };
        // `.json` is reachable only because the list says so.
        assert_eq!(hit(old, "data", true), None, "the old list must miss data.json");
        assert_eq!(
            hit(RoleExts::default(), "data", true),
            Some(tasks.join("data.json"))
        );
        // And the flip is the only thing deciding the setup pair, under either list.
        for exts in [old, RoleExts::default()] {
            assert_eq!(hit(exts, "setup", true), Some(tasks.join("setup")), "{exts:?}");
            assert_eq!(
                hit(exts, "setup", false),
                Some(tasks.join("setup.yml")),
                "{exts:?}"
            );
        }

        // .json and the extensionless form resolve at both ends of the flip.
        let out = resolve_src(&play, "- include_role: { name: r, tasks_from: data }\n");
        assert_eq!(
            first(&out, ReferenceKind::TasksFrom).targets,
            vec![tasks.join("data.json")]
        );
        let out = resolve_src(&play, "- include_role:\n    name: bare\n");
        assert_eq!(
            first(&out, ReferenceKind::Role).targets,
            vec![d.join("roles/bare/tasks/main")]
        );
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
