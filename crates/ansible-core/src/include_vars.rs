//! The `include_vars` action plugin, as a pure function: params + context + filesystem in,
//! the files Ansible would load (in order) and the variables they define out.
//!
//! Ported from `plugins/action/include_vars.py` (ansible-core 2.21.2) — behaviour, not code.
//! Every deviation from the Python is a comment below citing a live run. Filesystem access
//! goes through [`Fs`] so tests run on an in-memory tree.
//!
//! The free-form value (`include_vars: x.yml name=db`) is handled the way the engine does:
//! [`crate::splitter::parse_kv`] first, then the plugin's arg validation. Still out of
//! scope, by design: the `file:` search order (`_find_needle`, shared with T-015/T-038) —
//! a relative `file:` returns [`Outcome::NeedsNeedle`] instead of guessing.

use std::path::{Path, PathBuf};

use regex::Regex;

use crate::parse::{Document, Node};
use crate::splitter;

/// The module's dir-form and shared inputs, typed. Defaults mirror the plugin's.
/// `hash_behaviour` is deliberately absent: it changes merged *values*, never which
/// names get defined, and names are all this model tracks.
#[derive(Debug, Clone)]
pub struct Params {
    /// The free-form string, as written (`include_vars: <string>`). Parsed with
    /// `parse_kv(check_raw=false)` — `include_vars` is in `RAW_PARAM_MODULES` but not
    /// `FREEFORM_ACTIONS` — so `=` tokens become options and the remainder is the file.
    pub raw_params: Option<String>,
    pub file: Option<String>,
    pub dir: Option<String>,
    /// Namespace every loaded key under this one variable.
    pub name: Option<String>,
    /// 0 = unlimited.
    pub depth: u32,
    /// Python-re pattern a basename must contain (`re.search`) to be loaded.
    pub files_matching: Option<String>,
    /// Python-re patterns, end-anchored by the plugin (`re.search(rf"{p}$")`).
    pub ignore_files: Vec<String>,
    /// Extension allow-list, without dots.
    pub extensions: Vec<String>,
    /// `false` = a file with an unlisted extension fails the task, not just skipped.
    pub ignore_unknown_extensions: bool,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            raw_params: None,
            file: None,
            dir: None,
            name: None,
            depth: 0,
            files_matching: None,
            ignore_files: Vec::new(),
            extensions: ["yaml", "yml", "json"].map(String::from).to_vec(),
            ignore_unknown_extensions: false,
        }
    }
}

/// Where the task sits — the only two facts `_set_root_dir` reads.
pub struct Ctx<'a> {
    pub role_path: Option<&'a Path>,
    /// Directory of the file the task is written in (`origin.path` minus the filename).
    pub task_dir: &'a Path,
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Loaded(Loaded),
    /// The task fails at runtime; `message` mirrors the plugin's error text.
    Failed { message: String },
    /// In a role, `dir:` starts with `vars/`, and `<role>/<dir>` doesn't exist: the value
    /// is left unresolved and walks `<cwd>/<dir>` at runtime — statically unknowable.
    CwdFallback { relative: String },
    /// Relative `file:` — resolution is `_find_needle`, not this module's job.
    NeedsNeedle { file: String },
}

#[derive(Debug, PartialEq)]
pub struct Loaded {
    /// The computed root, dir form only — the one path `_set_root_dir` produces.
    pub dir: Option<PathBuf>,
    /// Every file loaded, in load order (dirs sorted by path, files sorted per dir).
    pub files: Vec<PathBuf>,
    /// Final top-level names, deduped in first-definition order; with `name:` this
    /// collapses to that single name.
    pub vars: Vec<String>,
}

/// This module's filesystem surface is the crate's — re-exported so the call sites
/// here keep reading `include_vars::Fs`. It used to be a second, narrower trait
/// declared right here; one door is the point (see [`crate::fs`]).
pub use crate::fs::{Fs, StdFs};

pub fn load(params: &Params, ctx: &Ctx, fs: &dyn Fs) -> Outcome {
    let mut depth_kv = None;
    let effective;
    let params = match &params.raw_params {
        Some(raw) => match apply_raw(params, raw) {
            Ok((p, d)) => {
                depth_kv = d;
                effective = p;
                &effective
            }
            Err(o) => return o,
        },
        None => params,
    };
    match (&params.dir, &params.file) {
        (Some(_), Some(_)) => fail(
            "You are mixing file only and dir only arguments, these are incompatible",
        ),
        (Some(dir), None) => load_dir(dir, params, depth_kv.as_deref(), ctx, fs),
        (None, Some(file)) => load_file(file, params, fs),
        (None, None) => fail("no file or dir specified"),
    }
}

fn fail(message: impl Into<String>) -> Outcome {
    Outcome::Failed { message: message.into() }
}

/// The free-form string, put through the same two stages the engine and plugin run:
/// `parse_kv`, then `run()`'s arg-name validation (unknown → mixing → typed errors, in
/// that order). Returns the effective params plus `depth` if it arrived as a k=v string —
/// that spelling always crashes at runtime and `load_dir` fails it at the walk, where the
/// plugin does.
fn apply_raw(base: &Params, raw: &str) -> Result<(Params, Option<String>), Outcome> {
    const DIR_ARGS: [&str; 6] =
        ["dir", "depth", "files_matching", "ignore_files", "extensions", "ignore_unknown_extensions"];
    let parsed = match splitter::parse_kv(raw, false) {
        Ok(p) => p,
        Err(m) => return Err(fail(m)),
    };

    let (mut dirs, mut files) = (0, 0);
    for (k, _) in &parsed.options {
        if DIR_ARGS.contains(&k.as_str()) {
            dirs += 1;
        } else if k == "file" {
            files += 1;
        } else if !matches!(k.as_str(), "name" | "hash_behaviour") {
            return Err(fail(format!("{k} is not a valid option in include_vars")));
        }
    }
    if parsed.raw_params.is_some() {
        files += 1; // `_raw_params` is a file-form argument
    }
    if dirs > 0 && files > 0 {
        return Err(fail(
            "You are mixing file only and dir only arguments, these are incompatible",
        ));
    }

    let mut p = base.clone();
    p.raw_params = None;
    let mut depth_kv = None;
    for (k, v) in &parsed.options {
        match k.as_str() {
            "dir" => p.dir = Some(v.clone()),
            "file" => p.file = Some(v.clone()),
            "name" => p.name = Some(v.clone()),
            "depth" => depth_kv = Some(v.clone()),
            "files_matching" => p.files_matching = Some(v.clone()),
            // The deprecated string form (removal 2.24): split on whitespace.
            "ignore_files" => p.ignore_files = v.split_whitespace().map(String::from).collect(),
            "extensions" => return Err(fail("The 'extensions' option must be a list.")),
            // Python truthiness on the raw string — live-verified: `=false` acts as true;
            // only the empty string (`ignore_unknown_extensions=`) is false.
            "ignore_unknown_extensions" => p.ignore_unknown_extensions = !v.is_empty(),
            _ => {} // hash_behaviour: accepted, irrelevant to which names get defined
        }
    }
    if p.file.is_none() && p.dir.is_none() {
        if let Some(r) = &parsed.raw_params {
            p.file = Some(r.trim_end_matches('\n').to_string());
        }
    }
    Ok((p, depth_kv))
}

fn load_dir(dir: &str, params: &Params, depth_kv: Option<&str>, ctx: &Ctx, fs: &dyn Fs) -> Outcome {
    let matcher = match &params.files_matching {
        Some(p) => match Regex::new(p) {
            Ok(m) => Some(m),
            Err(_) => return fail(format!("Invalid regular expression: '{p}'")),
        },
        None => None,
    };
    let mut ignore = Vec::new();
    for p in &params.ignore_files {
        match Regex::new(&format!("{p}$")) {
            Ok(m) => ignore.push(m),
            Err(_) => return fail(format!("Invalid regular expression: '{p}'")),
        }
    }

    let Some(root) = dir_root(dir, ctx, fs) else {
        return Outcome::CwdFallback { relative: dir.to_string() };
    };

    if !fs.exists(&root) {
        return fail(format!("{} directory does not exist", posix(&root)));
    }
    if !fs.is_dir(&root) {
        return fail(format!("{} is not a directory", posix(&root)));
    }
    // k=v `depth` is a string the plugin never converts; any non-empty value crashes the
    // first walk comparison — live-verified (`dir=v depth=1`). Empty is falsy -> 0.
    if depth_kv.is_some_and(|d| !d.is_empty()) {
        return fail("'>' not supported between instances of 'int' and '_AnsibleTaggedStr'");
    }

    let mut dirs = fs.walk(&root);
    dirs.sort_by(|a, b| a.0.cmp(&b.0));

    let mut files = Vec::new();
    let mut vars = Vec::new();
    for (d, mut names) in dirs {
        // Depth 1 is the root; a `continue` (not a prune) in the plugin too, so a
        // too-deep dir's own subdirs are still visited — same set either way.
        let rel_depth = d.strip_prefix(&root).map_or(0, |r| r.components().count()) as u32 + 1;
        if params.depth != 0 && rel_depth > params.depth {
            continue;
        }
        names.sort();
        for name in names {
            if matcher.as_ref().is_some_and(|m| !m.is_match(&name)) {
                continue;
            }
            if ignore.iter().any(|m| m.is_match(&name)) {
                continue;
            }
            // NOTE: no exclusion of the role's own vars/main.yml. The plugin has a guard
            // claiming to skip it, but its operands are crossed and it never fires —
            // live-verified on 2.21.2: `dir: vars` in a role loads vars/main.yml.
            if !valid_ext(&name, &params.extensions) {
                if params.ignore_unknown_extensions {
                    continue;
                }
                return fail(format!(
                    "'{}' does not have a valid extension: {}",
                    posix(&d.join(&name)),
                    params.extensions.join(", ")
                ));
            }
            let path = d.join(&name);
            match read_names(&path, fs) {
                Ok(names) => {
                    files.push(path);
                    for n in names {
                        if !vars.contains(&n) {
                            vars.push(n);
                        }
                    }
                }
                Err(o) => return o,
            }
        }
    }

    if let Some(name) = &params.name {
        vars = vec![name.clone()];
    }
    Outcome::Loaded(Loaded { dir: Some(root), files, vars })
}

/// `_set_root_dir`: the one computed path a `dir:` value means, or `None` for the in-role
/// `vars/`-prefixed miss whose runtime meaning is cwd-relative. Public so the resolver can
/// name the path in diagnostics without re-deriving it.
pub fn dir_root(dir: &str, ctx: &Ctx, fs: &dyn Fs) -> Option<PathBuf> {
    match ctx.role_path {
        Some(role) => {
            if dir.split('/').next() == Some("vars") {
                let candidate = role.join(dir);
                fs.exists(&candidate).then_some(candidate)
            } else {
                Some(role.join("vars").join(dir))
            }
        }
        None => Some(ctx.task_dir.join(dir)),
    }
}

/// [`Params`] from a task's args node: a bare scalar is the free-form line
/// (`raw_params`), a mapping fills the typed fields. `None` for anything else.
pub fn params_from_args(args: &Node) -> Option<Params> {
    let scalar = |k: &str| args.get(k).and_then(|n| n.as_str()).map(str::to_string);
    match args {
        Node::Scalar { value, .. } => {
            Some(Params { raw_params: Some(value.clone()), ..Params::default() })
        }
        Node::Mapping { .. } => {
            let mut p = Params {
                file: scalar("file"),
                dir: scalar("dir"),
                name: scalar("name"),
                files_matching: scalar("files_matching"),
                ..Params::default()
            };
            // YAML ints reach our Node as scalar text. A quoted "1" crashes at runtime
            // (no coercion in the plugin) — the provable-failure lint's problem, not ours.
            p.depth = scalar("depth").and_then(|d| d.parse().ok()).unwrap_or(0);
            if let Some(v) = scalar("ignore_unknown_extensions") {
                p.ignore_unknown_extensions = matches!(v.as_str(), "true" | "True" | "yes" | "on");
            }
            if let Some(n) = args.get("ignore_files") {
                p.ignore_files = string_list(n);
            }
            if let Some(n) = args.get("extensions") {
                let list = string_list(n);
                if !list.is_empty() {
                    p.extensions = list;
                }
            }
            Some(p)
        }
        _ => None,
    }
}

/// A list-typed option: a sequence of scalars, or the deprecated whitespace-split string.
fn string_list(n: &Node) -> Vec<String> {
    match n {
        Node::Sequence { .. } => n
            .items()
            .iter()
            .filter_map(|i| i.as_str().map(str::to_string))
            .collect(),
        Node::Scalar { value, .. } => value.split_whitespace().map(String::from).collect(),
        _ => Vec::new(),
    }
}

/// `os.path.isabs`, which is what the plugin calls — on the control node, which is POSIX,
/// so a leading `/` is absolute no matter which host we're analysing from. `Path::is_absolute`
/// alone answers "no" to `/etc/vars.yml` on Windows (it wants a drive prefix), which would
/// send a genuinely absolute `file:` down the relative branch.
fn is_absolute(path: &Path) -> bool {
    path.is_absolute() || path.to_string_lossy().starts_with('/')
}

fn load_file(file: &str, params: &Params, fs: &dyn Fs) -> Outcome {
    let path = Path::new(file);
    if !is_absolute(path) {
        return Outcome::NeedsNeedle { file: file.to_string() };
    }
    // `file:` skips extension validation — `_load_files` is called with its default
    // `validate_extensions=False` on this path.
    if !fs.exists(path) {
        return fail(format!("Unable to find '{file}'"));
    }
    match read_names(path, fs) {
        Ok(names) => {
            let vars = match &params.name {
                Some(n) => vec![n.clone()],
                None => names,
            };
            Outcome::Loaded(Loaded { dir: None, files: vec![path.to_path_buf()], vars })
        }
        Err(o) => o,
    }
}

/// The plugin builds these message paths with `os.path.join` on the control node, so they
/// read with `/` — [`crate::posix_display`] keeps our replicas byte-identical on Windows.
use crate::posix_display as posix;

/// Python `splitext` semantics via `Path::extension`: both call `.hidden` extensionless.
fn valid_ext(name: &str, extensions: &[String]) -> bool {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| extensions.iter().any(|x| x == e))
}

/// A vars file's top-level keys. Empty and null documents load as zero vars (the plugin
/// maps `data is None` to `{}`); any other non-mapping top level fails the task.
fn read_names(path: &Path, fs: &dyn Fs) -> Result<Vec<String>, Outcome> {
    let Some(text) = fs.read(path) else {
        return Err(Outcome::Failed { message: format!("Unable to read '{}'", posix(path)) });
    };
    let Some(nodes) = Document::new(text).parse() else {
        return Err(Outcome::Failed { message: format!("failed to parse '{}'", posix(path)) });
    };
    let mut out = Vec::new();
    for n in &nodes {
        match n {
            Node::Mapping { entries, .. } => {
                out.extend(entries.iter().filter_map(|(k, _)| k.as_str().map(String::from)));
            }
            Node::Scalar { value, .. }
                if matches!(value.as_str(), "" | "~" | "null" | "Null" | "NULL") => {}
            _ => {
                return Err(Outcome::Failed {
                    message: format!("'{}' must be stored as a dictionary/hash", posix(path)),
                });
            }
        }
    }
    Ok(out)
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// In-memory tree: path -> content. Directories exist implicitly as prefixes.
    struct MemFs(BTreeMap<PathBuf, String>);

    impl MemFs {
        fn new(files: &[(&str, &str)]) -> Self {
            Self(files.iter().map(|(p, c)| (PathBuf::from(p), c.to_string())).collect())
        }
    }

    impl Fs for MemFs {
        fn kind(&self, p: &Path) -> Option<crate::fs::Kind> {
            if self.0.contains_key(p) {
                Some(crate::fs::Kind::File)
            } else if self.0.keys().any(|k| k.starts_with(p) && k != p) {
                Some(crate::fs::Kind::Dir)
            } else {
                None
            }
        }
        fn read(&self, p: &Path) -> Option<String> {
            self.0.get(p).cloned()
        }
        fn read_dir(&self, p: &Path) -> Vec<(PathBuf, crate::fs::Kind)> {
            self.0
                .keys()
                .filter(|k| k.parent() == Some(p))
                .map(|k| (k.clone(), crate::fs::Kind::File))
                .collect()
        }
        /// Nothing in an in-memory tree is a symlink, so a path is its own identity.
        fn canonical(&self, p: &Path) -> Option<PathBuf> {
            self.exists(p).then(|| p.to_path_buf())
        }
        fn walk(&self, root: &Path) -> Vec<(PathBuf, Vec<String>)> {
            let mut dirs: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
            dirs.insert(root.to_path_buf(), Vec::new());
            for k in self.0.keys().filter(|k| k.starts_with(root)) {
                let dir = k.parent().unwrap().to_path_buf();
                // Every intermediate dir between root and the file exists too.
                let mut d = dir.clone();
                while d != *root {
                    dirs.entry(d.clone()).or_default();
                    d = d.parent().unwrap().to_path_buf();
                }
                dirs.entry(dir)
                    .or_default()
                    .push(k.file_name().unwrap().to_str().unwrap().to_string());
            }
            dirs.into_iter().collect()
        }
    }

    fn in_role<'a>() -> Ctx<'a> {
        Ctx { role_path: Some(Path::new("/repo/roles/db")), task_dir: Path::new("/repo/roles/db/tasks") }
    }

    fn no_role<'a>() -> Ctx<'a> {
        Ctx { role_path: None, task_dir: Path::new("/repo/playbooks/setup") }
    }

    fn dir_params(dir: &str) -> Params {
        Params { dir: Some(dir.to_string()), ..Params::default() }
    }

    fn loaded(o: Outcome) -> Loaded {
        match o {
            Outcome::Loaded(l) => l,
            other => panic!("expected Loaded, got {other:?}"),
        }
    }

    #[test]
    fn in_role_unprefixed_resolves_under_role_vars() {
        let fs = MemFs::new(&[("/repo/roles/db/vars/prod/a.yml", "foo: 1\n")]);
        let l = loaded(load(&dir_params("prod"), &in_role(), &fs));
        assert_eq!(l.dir.unwrap(), Path::new("/repo/roles/db/vars/prod"));
        assert_eq!(l.vars, ["foo"]);
    }

    #[test]
    fn in_role_vars_prefix_is_not_doubled() {
        let fs = MemFs::new(&[("/repo/roles/db/vars/prod/a.yml", "foo: 1\n")]);
        let l = loaded(load(&dir_params("vars/prod"), &in_role(), &fs));
        assert_eq!(l.dir.unwrap(), Path::new("/repo/roles/db/vars/prod"));
    }

    #[test]
    fn in_role_vars_prefix_missing_falls_back_to_cwd() {
        let fs = MemFs::new(&[("/repo/roles/db/vars/production/a.yml", "foo: 1\n")]);
        let o = load(&dir_params("vars/prod"), &in_role(), &fs);
        assert_eq!(o, Outcome::CwdFallback { relative: "vars/prod".into() });
    }

    #[test]
    fn in_role_unprefixed_missing_fails_at_the_computed_path() {
        let fs = MemFs::new(&[("/repo/roles/db/vars/production/a.yml", "foo: 1\n")]);
        let o = load(&dir_params("prod"), &in_role(), &fs);
        assert_eq!(
            o,
            Outcome::Failed {
                message: "/repo/roles/db/vars/prod directory does not exist".into()
            }
        );
    }

    #[test]
    fn outside_role_resolves_from_the_task_files_dir() {
        // Not the playbook dir, not the project root — the task file's own directory.
        let fs = MemFs::new(&[("/repo/playbooks/setup/settings/a.yml", "foo: 1\n")]);
        let l = loaded(load(&dir_params("settings"), &no_role(), &fs));
        assert_eq!(l.dir.unwrap(), Path::new("/repo/playbooks/setup/settings"));
    }

    #[test]
    fn path_that_is_a_file_fails_as_not_a_directory() {
        let fs = MemFs::new(&[("/repo/playbooks/setup/settings", "foo: 1\n")]);
        let o = load(&dir_params("settings"), &no_role(), &fs);
        assert_eq!(
            o,
            Outcome::Failed { message: "/repo/playbooks/setup/settings is not a directory".into() }
        );
    }

    #[test]
    fn files_load_sorted_dirs_first_by_path_then_names() {
        let fs = MemFs::new(&[
            ("/r/tasks/v/b.yml", "b: 1\n"),
            ("/r/tasks/v/a.yml", "a: 1\n"),
            ("/r/tasks/v/sub/c.yml", "c: 1\n"),
        ]);
        let ctx = Ctx { role_path: None, task_dir: Path::new("/r/tasks") };
        let l = loaded(load(&dir_params("v"), &ctx, &fs));
        let got: Vec<_> = l.files.iter().map(|p| posix(p)).collect();
        assert_eq!(got, ["/r/tasks/v/a.yml", "/r/tasks/v/b.yml", "/r/tasks/v/sub/c.yml"]);
    }

    #[test]
    fn depth_one_stops_at_the_root() {
        let fs = MemFs::new(&[
            ("/r/tasks/v/a.yml", "a: 1\n"),
            ("/r/tasks/v/sub/c.yml", "c: 1\n"),
        ]);
        let ctx = Ctx { role_path: None, task_dir: Path::new("/r/tasks") };
        let params = Params { depth: 1, ..dir_params("v") };
        let l = loaded(load(&params, &ctx, &fs));
        assert_eq!(l.vars, ["a"]);
    }

    #[test]
    fn unknown_extension_fails_the_task_by_default() {
        let fs = MemFs::new(&[
            ("/r/tasks/v/README.md", "hi\n"),
            ("/r/tasks/v/a.yml", "a: 1\n"),
        ]);
        let ctx = Ctx { role_path: None, task_dir: Path::new("/r/tasks") };
        let o = load(&dir_params("v"), &ctx, &fs);
        assert_eq!(
            o,
            Outcome::Failed {
                message: "'/r/tasks/v/README.md' does not have a valid extension: yaml, yml, json"
                    .into()
            }
        );
    }

    #[test]
    fn unknown_extension_is_skipped_when_ignored() {
        let fs = MemFs::new(&[
            ("/r/tasks/v/README.md", "hi\n"),
            ("/r/tasks/v/a.yml", "a: 1\n"),
        ]);
        let ctx = Ctx { role_path: None, task_dir: Path::new("/r/tasks") };
        let params = Params { ignore_unknown_extensions: true, ..dir_params("v") };
        let l = loaded(load(&params, &ctx, &fs));
        assert_eq!(l.vars, ["a"]);
    }

    /// The upstream docs bug pinned: entries are end-anchored regexes, so `bastion.yaml`
    /// also swallows `edge-bastion.yaml`.
    #[test]
    fn ignore_files_entries_are_end_anchored_regexes() {
        let fs = MemFs::new(&[
            ("/r/tasks/v/bastion.yaml", "a: 1\n"),
            ("/r/tasks/v/edge-bastion.yaml", "b: 1\n"),
            ("/r/tasks/v/keep.yaml", "c: 1\n"),
        ]);
        let ctx = Ctx { role_path: None, task_dir: Path::new("/r/tasks") };
        let params = Params { ignore_files: vec!["bastion.yaml".into()], ..dir_params("v") };
        let l = loaded(load(&params, &ctx, &fs));
        assert_eq!(l.vars, ["c"]);
    }

    #[test]
    fn files_matching_is_an_unanchored_search_on_the_basename() {
        let fs = MemFs::new(&[
            ("/r/tasks/v/db-prod.yml", "a: 1\n"),
            ("/r/tasks/v/web.yml", "b: 1\n"),
        ]);
        let ctx = Ctx { role_path: None, task_dir: Path::new("/r/tasks") };
        let params = Params { files_matching: Some("prod".into()), ..dir_params("v") };
        let l = loaded(load(&params, &ctx, &fs));
        assert_eq!(l.vars, ["a"]);
    }

    #[test]
    fn invalid_ignore_files_regex_fails_like_the_plugin() {
        let fs = MemFs::new(&[("/r/tasks/v/a.yml", "a: 1\n")]);
        let ctx = Ctx { role_path: None, task_dir: Path::new("/r/tasks") };
        let params = Params { ignore_files: vec!["[".into()], ..dir_params("v") };
        assert_eq!(
            load(&params, &ctx, &fs),
            Outcome::Failed { message: "Invalid regular expression: '['".into() }
        );
    }

    #[test]
    fn name_wraps_everything_under_one_variable() {
        let fs = MemFs::new(&[("/r/tasks/v/a.yml", "foo: 1\nbar: 2\n")]);
        let ctx = Ctx { role_path: None, task_dir: Path::new("/r/tasks") };
        let params = Params { name: Some("db".into()), ..dir_params("v") };
        let l = loaded(load(&params, &ctx, &fs));
        assert_eq!(l.vars, ["db"]);
    }

    #[test]
    fn empty_dir_and_empty_file_both_load_as_nothing() {
        let fs = MemFs::new(&[("/r/tasks/v/empty.yml", "")]);
        let ctx = Ctx { role_path: None, task_dir: Path::new("/r/tasks") };
        let l = loaded(load(&dir_params("v"), &ctx, &fs));
        assert_eq!(l.files.len(), 1);
        assert!(l.vars.is_empty());
    }

    #[test]
    fn non_mapping_file_fails_as_not_a_dictionary() {
        let fs = MemFs::new(&[("/r/tasks/v/list.yml", "- a\n- b\n")]);
        let ctx = Ctx { role_path: None, task_dir: Path::new("/r/tasks") };
        assert_eq!(
            load(&dir_params("v"), &ctx, &fs),
            Outcome::Failed {
                message: "'/r/tasks/v/list.yml' must be stored as a dictionary/hash".into()
            }
        );
    }

    /// Live-verified on 2.21.2: the plugin's "never include main.yml" guard never fires.
    #[test]
    fn role_vars_main_yml_is_loaded_despite_the_upstream_comment() {
        let fs = MemFs::new(&[
            ("/repo/roles/db/vars/main.yml", "from_main: 1\n"),
            ("/repo/roles/db/vars/other.yml", "from_other: 2\n"),
        ]);
        let l = loaded(load(&dir_params("vars"), &in_role(), &fs));
        assert_eq!(l.vars, ["from_main", "from_other"]);
    }

    #[test]
    fn mixing_file_and_dir_is_the_plugins_error() {
        let fs = MemFs::new(&[]);
        let params = Params {
            file: Some("a.yml".into()),
            ..dir_params("v")
        };
        assert_eq!(
            load(&params, &no_role(), &fs),
            Outcome::Failed {
                message: "You are mixing file only and dir only arguments, these are incompatible"
                    .into()
            }
        );
    }

    #[test]
    fn relative_file_defers_to_find_needle() {
        let fs = MemFs::new(&[]);
        let params = Params { file: Some("x.yml".into()), ..Params::default() };
        assert_eq!(
            load(&params, &no_role(), &fs),
            Outcome::NeedsNeedle { file: "x.yml".into() }
        );
    }

    fn raw(s: &str) -> Params {
        Params { raw_params: Some(s.to_string()), ..Params::default() }
    }

    #[test]
    fn free_form_kv_options_ride_along() {
        // `include_vars: x.yml name=db` — live-verified equivalent of {file, name}.
        let fs = MemFs::new(&[("/abs/x.yml", "foo: 1\n")]);
        let l = loaded(load(&raw("/abs/x.yml name=db"), &no_role(), &fs));
        assert_eq!(l.files, [PathBuf::from("/abs/x.yml")]);
        assert_eq!(l.vars, ["db"]);
    }

    #[test]
    fn free_form_relative_remainder_defers_like_file() {
        let fs = MemFs::new(&[]);
        assert_eq!(
            load(&raw("x.yml name=db"), &no_role(), &fs),
            Outcome::NeedsNeedle { file: "x.yml".into() }
        );
    }

    #[test]
    fn free_form_dir_kv_flips_to_the_dir_form() {
        let fs = MemFs::new(&[("/r/tasks/v/a.yml", "a: 1\n")]);
        let ctx = Ctx { role_path: None, task_dir: Path::new("/r/tasks") };
        let l = loaded(load(&raw("dir=v name=db"), &ctx, &fs));
        assert_eq!(l.dir.as_deref(), Some(Path::new("/r/tasks/v")));
        assert_eq!(l.vars, ["db"]);
    }

    /// The T-046 pinned test, quoting the real error: a bare path containing `=` is
    /// split as k=v and the prefix is rejected as an option.
    #[test]
    fn free_form_path_with_equals_is_the_live_error() {
        let fs = MemFs::new(&[]);
        assert_eq!(
            load(&raw("vars/we=ird.yml"), &no_role(), &fs),
            Outcome::Failed { message: "vars/we is not a valid option in include_vars".into() }
        );
    }

    #[test]
    fn free_form_bare_remainder_plus_dir_arg_is_mixing() {
        let fs = MemFs::new(&[("/r/tasks/v/a.yml", "a: 1\n")]);
        let ctx = Ctx { role_path: None, task_dir: Path::new("/r/tasks") };
        assert_eq!(
            load(&raw("foo.yml dir=v"), &ctx, &fs),
            Outcome::Failed {
                message: "You are mixing file only and dir only arguments, these are incompatible"
                    .into()
            }
        );
    }

    #[test]
    fn free_form_extensions_string_fails_like_the_plugin() {
        let fs = MemFs::new(&[("/r/tasks/v/a.yml", "a: 1\n")]);
        let ctx = Ctx { role_path: None, task_dir: Path::new("/r/tasks") };
        assert_eq!(
            load(&raw("dir=v extensions=json"), &ctx, &fs),
            Outcome::Failed { message: "The 'extensions' option must be a list.".into() }
        );
    }

    /// Live-verified: k=v `depth` stays a string and the walk's int comparison crashes —
    /// but only after the directory checks, so a missing dir still reports as missing.
    #[test]
    fn free_form_depth_string_crashes_after_dir_checks() {
        let fs = MemFs::new(&[("/r/tasks/v/a.yml", "a: 1\n")]);
        let ctx = Ctx { role_path: None, task_dir: Path::new("/r/tasks") };
        assert_eq!(
            load(&raw("dir=v depth=1"), &ctx, &fs),
            Outcome::Failed {
                message: "'>' not supported between instances of 'int' and '_AnsibleTaggedStr'"
                    .into()
            }
        );
        assert_eq!(
            load(&raw("dir=missing depth=1"), &ctx, &fs),
            Outcome::Failed { message: "/r/tasks/missing directory does not exist".into() }
        );
    }

    /// Live-verified: `ignore_unknown_extensions=false` is Python-truthy, i.e. true.
    #[test]
    fn free_form_bool_string_false_acts_as_true() {
        let fs = MemFs::new(&[
            ("/r/tasks/v/a.yml", "a: 1\n"),
            ("/r/tasks/v/README.md", "hi\n"),
        ]);
        let ctx = Ctx { role_path: None, task_dir: Path::new("/r/tasks") };
        let l = loaded(load(&raw("dir=v ignore_unknown_extensions=false"), &ctx, &fs));
        assert_eq!(l.vars, ["a"]);
    }

    #[test]
    fn free_form_ignore_files_string_splits_on_whitespace() {
        let fs = MemFs::new(&[
            ("/r/tasks/v/a.yml", "a: 1\n"),
            ("/r/tasks/v/b.yml", "b: 1\n"),
        ]);
        let ctx = Ctx { role_path: None, task_dir: Path::new("/r/tasks") };
        let l = loaded(load(&raw("dir=v ignore_files=a.yml"), &ctx, &fs));
        assert_eq!(l.vars, ["b"]);
    }

    #[test]
    fn absolute_file_loads_without_extension_validation() {
        // `file:` never validates extensions — only the dir walk does.
        let fs = MemFs::new(&[("/abs/vars.conf", "foo: 1\n")]);
        let params = Params { file: Some("/abs/vars.conf".into()), ..Params::default() };
        let l = loaded(load(&params, &no_role(), &fs));
        assert_eq!(l.vars, ["foo"]);
        assert_eq!(l.dir, None);
    }
}
