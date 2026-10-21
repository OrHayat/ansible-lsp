//! The reverse index (T-020): target file -> every reference in the workspace that reaches it.
//!
//! Everything else the resolver does is forward-only — *this line points there*. This is
//! the inversion, built from the same `(Reference, Resolution)` pairs the scan already
//! computes, so it costs one map insert per edge and no extra read or parse.
//!
//! An edge is one reference *line*, not one file pair: two `include_tasks` of the same file
//! are two edges, because the consumers want the site (a line number, a kind, whether it
//! was templated), and the coarse "which files depend on B" is a dedupe on top of that.
//!
//! A templated reference contributes an edge to **every** candidate the resolver offered.
//! Overcounting is the safe direction: it can only make a file look used when it might not
//! be, and a false "unused" is worse than a missed one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::fs::Fs;
use crate::parse::{Document, Span};
use crate::references::{Reference, ReferenceKind};
use crate::resolve::Resolution;

/// The `apply:` block on the include that produced an edge (T-167).
///
/// Its `vars:` bind on the tasks the include brings in, so they are definitions belonging to
/// the *target* file while being written in the source — which is why they travel on the
/// edge and not in either file's own walk. Shared behind an `Arc` between every edge one
/// reference produced, so a templated include reaching twenty candidates carries one copy.
#[derive(Debug, PartialEq, Eq)]
pub struct ApplySite {
    /// (name, span of the value) in written order.
    pub vars: Vec<(String, Span)>,
    /// The whole `apply:` value's range in the source file — outside it, in that file,
    /// these names are not defined.
    pub span: Span,
    /// The including task's `when:`, which decides whether the include happens at all and
    /// therefore whether these bind. Kept so a hover says "defined only when …" rather
    /// than claiming a guarded definition unconditionally.
    pub conditions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    /// The file the reference is written in, canonical. Shared between every edge of one
    /// source so the index costs a pointer per edge rather than a path.
    pub source: Arc<Path>,
    /// The reference's value in `source`, in bytes.
    pub span: Span,
    /// 0-based line of `span.start`, computed while the source text was in hand so a reader
    /// never has to re-open the file to say where the edge is.
    pub line: u32,
    pub kind: ReferenceKind,
    /// The value as written, which is what Ansible's include retry reads (T-248).
    pub value: Arc<str>,
    /// The value carried a Jinja expression, so this is one of the candidates that line may
    /// reach rather than the one file it names.
    pub templated: bool,
    /// The `apply:` on this include, when it wrote any `vars:` (T-167).
    pub apply: Option<Arc<ApplySite>>,
}

/// Keys are canonical absolute paths, so two spellings of one target collapse.
#[derive(Debug, Default)]
pub struct ReverseIndex {
    by_target: HashMap<PathBuf, Vec<Edge>>,
    /// Each source's targets, so replacing a source's edges is a walk of its own list and
    /// not of the whole index.
    by_source: HashMap<PathBuf, Vec<PathBuf>>,
}

/// The canonical form of a path for a key, for callers with no [`Fs`] in hand — a query
/// from the editor, a test. The index itself is built through [`edges_of`], which goes via
/// the caller's `Fs` so a [`crate::cache::ScanCache`] memoizes the syscall: measured on
/// `demo/`, canonicalising per edge here instead cost the scan 35 -> 46 ms.
pub fn canon(p: &Path) -> PathBuf {
    crate::fs::StdFs.canonical(p).unwrap_or_else(|| p.to_path_buf())
}

/// One file's outgoing edges, keyed by its canonical path — what [`ReverseIndex::replace`]
/// takes.
#[derive(Debug)]
pub struct SourceEdges {
    pub source: PathBuf,
    pub edges: Vec<(PathBuf, Edge)>,
}

/// The edges one file contributes: one per target the resolver offered for each of its
/// references. A `vars_files` group anchor spans the whole first-match list and resolves to
/// the same file its winning alternative does, so it is skipped rather than counted twice.
pub fn edges_of(fs: &dyn Fs, source: &Path, refs: &[(Reference, Resolution)], doc: &Document) -> SourceEdges {
    let key = |p: &Path| fs.canonical(p).unwrap_or_else(|| p.to_path_buf());
    let source = key(source);
    let shared: Arc<Path> = Arc::from(source.as_path());
    let mut edges = Vec::new();
    for (r, res) in refs {
        if r.vars_files_group.is_some() {
            continue;
        }
        let (line, _) = doc.byte_to_lsp(r.span.start);
        let apply = match (r.apply_vars.is_empty(), r.apply_span) {
            (false, Some(span)) => Some(Arc::new(ApplySite {
                vars: r.apply_vars.clone(),
                span,
                conditions: r.conditions.clone(),
            })),
            _ => None,
        };
        let value: Arc<str> = Arc::from(r.value.as_str());
        for target in &res.targets {
            edges.push((
                key(target),
                Edge {
                    source: shared.clone(),
                    span: r.span,
                    line,
                    kind: r.kind,
                    value: value.clone(),
                    templated: r.templated,
                    apply: apply.clone(),
                },
            ));
        }
    }
    SourceEdges { source, edges }
}

impl ReverseIndex {
    /// Set the source's outgoing edges to exactly these, dropping any it had before. An
    /// empty list is how a file that no longer parses leaves the index.
    pub fn replace(&mut self, SourceEdges { source: key, edges }: SourceEdges) {
        self.remove_canonical(&key);
        if edges.is_empty() {
            return;
        }
        let mut targets: Vec<PathBuf> = Vec::new();
        for (target, edge) in edges {
            let list = self.by_target.entry(target.clone()).or_default();
            list.push(edge);
            list.sort_by(|a, b| a.source.cmp(&b.source).then(a.line.cmp(&b.line)).then(a.span.start.cmp(&b.span.start)));
            if !targets.contains(&target) {
                targets.push(target);
            }
        }
        self.by_source.insert(key, targets);
    }

    pub fn remove(&mut self, source: &Path) {
        self.remove_canonical(&canon(source));
    }

    fn remove_canonical(&mut self, key: &Path) {
        let Some(targets) = self.by_source.remove(key) else { return };
        for t in targets {
            if let Some(list) = self.by_target.get_mut(&t) {
                list.retain(|e| &*e.source != key);
                if list.is_empty() {
                    self.by_target.remove(&t);
                }
            }
        }
    }

    /// Drop every source `keep` rejects. The workspace scan uses it to forget files that
    /// were deleted, or stopped parsing, since the last pass.
    pub fn retain_sources(&mut self, keep: impl Fn(&Path) -> bool) {
        let gone: Vec<PathBuf> = self.by_source.keys().filter(|s| !keep(s)).cloned().collect();
        for s in gone {
            self.remove_canonical(&s);
        }
    }

    /// Every reference reaching `target`, ordered by source path then line.
    pub fn inbound(&self, target: &Path) -> &[Edge] {
        self.by_target.get(&canon(target)).map_or(&[], Vec::as_slice)
    }

    pub fn sources(&self) -> impl Iterator<Item = &Path> {
        self.by_source.keys().map(PathBuf::as_path)
    }

    /// Every target with its inbound edges, in path order — the shape `scan` prints.
    pub fn targets(&self) -> Vec<(&Path, &[Edge])> {
        let mut v: Vec<(&Path, &[Edge])> =
            self.by_target.iter().map(|(t, e)| (t.as_path(), e.as_slice())).collect();
        v.sort_by(|a, b| a.0.cmp(b.0));
        v
    }

    /// Edges in total.
    pub fn len(&self) -> usize {
        self.by_target.values().map(Vec::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.by_target.is_empty()
    }
}

/// One way Ansible reaches a file in a role's `tasks/` or `handlers/`, as its include retry
/// sees it (T-248, `included_file.py:136-180`, `helpers.py:158-198`).
///
/// When an include in a role file misses, Ansible walks the file's include parents and
/// retries under `<anchor>/<cum>`, where `<cum>` joins the directories those parents
/// *wrote* — not where their files sit. Measured on 2.21.3: `sub/a.yml` writing
/// `deep/b.yml` loads `tasks/deep/b.yml`, and an include in it then retries under `deep/`
/// and `sub/deep/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chain {
    /// `<cum>` per include or import above the file, nearest first, relative to the anchor so
    /// the resolver joins it onto the context's own spelling of that folder. Empty when the
    /// file is entered as a role's entry point — `roles:`, `tasks_from`, or its `main.yml`.
    ///
    /// Not modelled: past an `include_role` that is itself below an include, Ansible goes
    /// on walking that include's parents with odd paths (`role/<cum>`, then the outer
    /// include's own directory). Each needs a file at a place nobody writes one.
    pub cums: Vec<PathBuf>,
    /// The reference this way starts from, as (file, 0-based line). `None` for a `main.yml`
    /// no reference in the workspace names.
    pub via: Option<(Arc<Path>, u32)>,
}

/// An index of only the edges [`ReverseIndex::chains`] reads — task includes, imports and
/// role entries — over `files`, each resolved file-locally. What a caller with no complete
/// index yet builds first, so that its real pass can resolve role files through
/// [`ReverseIndex::chains_in`].
pub fn ways_in(cache: &crate::cache::ScanCache, files: &[PathBuf]) -> ReverseIndex {
    let mut idx = ReverseIndex::default();
    for path in files {
        let Some(src) = cache.source(path) else { continue };
        let Some(nodes) = src.nodes.as_deref() else { continue };
        let ctx = cache.context(path);
        let extracted = crate::references::extract(nodes);
        let resolver = crate::resolve::Resolver { fs: cache, in_playbook: extracted.in_playbook, ..Default::default() };
        let refs: Vec<_> = extracted
            .refs
            .into_iter()
            .filter(|r| {
                matches!(
                    r.kind,
                    ReferenceKind::IncludeTasks | ReferenceKind::ImportTasks | ReferenceKind::Role | ReferenceKind::TasksFrom
                )
            })
            .map(|r| {
                let res = resolver.resolve(&r, &ctx);
                (r, res)
            })
            .collect();
        idx.replace(edges_of(cache, path, &refs, &Document::new(src.text.to_string())));
    }
    idx
}

/// Past this many ways into one file the answer is "not known", never a guess.
const MAX_CHAINS: usize = 64;

impl ReverseIndex {
    /// Every way into `file`, a role file under `anchor` (its role's `tasks/` or
    /// `handlers/`), both canonical. `None` when any way in is unknowable — a templated or
    /// absolute include, an includer outside `anchor`, a cycle, too many ways — or when
    /// nothing reaches the file at all, so the caller keeps its file-local answer.
    pub fn chains(&self, file: &Path, anchor: &Path) -> Option<Vec<Chain>> {
        if !file.starts_with(anchor) {
            return None;
        }
        let ways = self.ways(file, anchor, &mut Vec::new())?;
        Some(ways.into_iter().map(|(cums, via)| Chain { cums, via }).collect())
    }

    /// [`Self::chains`] for `file` where `ctx` places it, both canonicalised through `fs`.
    pub fn chains_in(&self, fs: &dyn Fs, file: &Path, ctx: &crate::workspace::FileContext) -> Option<Vec<Chain>> {
        let anchor = fs.canonical(ctx.role_anchor_dir.as_deref()?)?;
        let file = fs.canonical(file).unwrap_or_else(|| file.to_path_buf());
        self.chains(&file, &anchor)
    }

    #[allow(clippy::type_complexity)]
    fn ways(&self, file: &Path, anchor: &Path, seen: &mut Vec<PathBuf>) -> Option<Vec<(Vec<PathBuf>, Option<(Arc<Path>, u32)>)>> {
        if seen.iter().any(|s| s == file) {
            return None;
        }
        seen.push(file.to_path_buf());
        let mut out: Vec<(Vec<PathBuf>, Option<(Arc<Path>, u32)>)> = Vec::new();
        for e in self.by_target.get(file).map_or(&[][..], Vec::as_slice) {
            match e.kind {
                ReferenceKind::Role | ReferenceKind::TasksFrom => {
                    out.push((Vec::new(), Some((e.source.clone(), e.line))));
                }
                ReferenceKind::IncludeTasks | ReferenceKind::ImportTasks => {
                    let written = Path::new(&*e.value);
                    if e.templated || written.is_absolute() || !e.source.starts_with(anchor) {
                        return None;
                    }
                    let dir = written.parent().unwrap_or(Path::new(""));
                    for (above, _) in self.ways(&e.source, anchor, seen)? {
                        let mut cums = vec![dir.to_path_buf()];
                        cums.extend(above.iter().map(|c| c.join(dir)));
                        out.push((cums, Some((e.source.clone(), e.line))));
                    }
                }
                _ => {}
            }
            if out.len() > MAX_CHAINS {
                return None;
            }
        }
        let is_main = file.parent() == Some(anchor)
            && matches!(file.file_name().and_then(|n| n.to_str()), Some("main.yml" | "main.yaml" | "main"));
        if is_main {
            out.push((Vec::new(), None));
        }
        // One way per set of folders, the first that names a line: two ways with the same
        // folders search the same places, and keeping both doubles everything below.
        let mut kept: Vec<(Vec<PathBuf>, Option<(Arc<Path>, u32)>)> = Vec::new();
        for way in out {
            if !kept.iter().any(|k| k.0 == way.0) {
                kept.push(way);
            }
        }
        seen.pop();
        (!kept.is_empty()).then_some(kept)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::ScanCache;
    use crate::references::extract;
    use crate::resolve::Resolver;

    /// Resolve one on-disk file the way both consumers do, and return its edges.
    fn edges(root: &Path, rel: &str) -> SourceEdges {
        let path = root.join(rel);
        let cache = ScanCache::default();
        let src = cache.source(&path).expect("fixture readable");
        let doc = Document::new(src.text.to_string());
        let nodes = src.nodes.clone().expect("fixture parses");
        let ctx = cache.context(&path);
        let extracted = extract(&nodes);
        let resolver = Resolver { fs: &cache, in_playbook: extracted.in_playbook, ..Default::default() };
        let refs: Vec<(Reference, Resolution)> =
            extracted.refs.into_iter().map(|r| { let res = resolver.resolve(&r, &ctx); (r, res) }).collect();
        edges_of(&cache, &path, &refs, &doc)
    }

    /// The `apply: vars:` names each inbound edge of `rel` carries, with the text each
    /// value span covers — the shape the server turns into definitions.
    fn applied(idx: &ReverseIndex, root: &Path, rel: &str, src: &str) -> Vec<(String, String)> {
        idx.inbound(&canon(root).join(rel))
            .iter()
            .filter_map(|e| e.apply.as_ref())
            .flat_map(|a| a.vars.iter().map(|(n, s)| (n.clone(), s.slice(src).to_string())))
            .collect()
    }

    /// T-248: the folders each way in wrote, nearest first, and `None` wherever one way in
    /// cannot be known — which keeps the file-local search for the whole file.
    #[test]
    fn the_ways_into_a_role_file_carry_the_folders_their_includes_wrote() {
        let root = crate::testing::project(
            "t248-chains",
            "[defaults]\n",
            &[
                ("site.yml", "- hosts: all\n  roles: [r]\n"),
                ("roles/r/tasks/main.yml", "- include_tasks: sub/a.yml\n- include_tasks: \"loo{{ pick }}.yml\"\n"),
                ("roles/r/tasks/sub/a.yml", "- include_tasks: deep/b.yml\n- include_tasks: c.yml\n"),
                ("roles/r/tasks/deep/b.yml", "- debug: {msg: b}\n"),
                ("roles/r/tasks/c.yml", "- include_tasks: d.yml\n"),
                ("roles/r/tasks/d.yml", "- include_tasks: c.yml\n"),
                ("roles/r/tasks/loose.yml", "- debug: {msg: loose}\n"),
                ("roles/r/handlers/main.yml", "- include_tasks: h.yml\n"),
                ("roles/r/handlers/h.yml", "- debug: {msg: h}\n"),
            ],
        );
        let cache = ScanCache::default();
        let files: Vec<PathBuf> = [
            "site.yml", "roles/r/tasks/main.yml", "roles/r/tasks/sub/a.yml", "roles/r/tasks/deep/b.yml",
            "roles/r/tasks/c.yml", "roles/r/tasks/d.yml", "roles/r/tasks/loose.yml", "roles/r/handlers/main.yml",
            "roles/r/handlers/h.yml",
        ]
        .iter()
        .map(|f| root.join(f))
        .collect();
        let idx = ways_in(&cache, &files);
        let r = canon(&root.join("roles/r"));
        let ways = |rel: &str, dir: &str| -> Option<Vec<Vec<String>>> {
            Some(
                idx.chains(&r.join(rel), &r.join(dir))?
                    .into_iter()
                    .map(|c| c.cums.iter().map(|p| crate::posix_display(p)).collect())
                    .collect(),
            )
        };
        assert_eq!(ways("tasks/deep/b.yml", "tasks"), Some(vec![vec!["deep".into(), "sub/deep".into()]]));
        assert_eq!(ways("tasks/main.yml", "tasks"), Some(vec![vec![]]), "an entry, once");
        let main = idx.chains(&r.join("tasks/main.yml"), &r.join("tasks")).unwrap();
        assert_eq!(main[0].via.as_ref().map(|v| v.1), Some(1), "named by the `roles:` line, not as an unnamed entry");
        assert_eq!(ways("handlers/h.yml", "handlers"), Some(vec![vec![String::new()]]));
        assert_eq!(idx.inbound(&r.join("tasks/loose.yml")).len(), 1, "the templated include reaches it");
        assert_eq!(ways("tasks/loose.yml", "tasks"), None, "through a templated include");
        assert_eq!(ways("tasks/c.yml", "tasks"), None, "on a cycle");
        assert_eq!(ways("handlers/h.yml", "tasks"), None, "not under the anchor");
    }

    #[test]
    fn an_apply_vars_entry_rides_the_edge_to_the_file_it_binds_on() {
        const PLAY: &str = "- hosts: all\n  tasks:\n    - include_tasks:\n        file: inc.yml\n        apply:\n          vars:\n            applied: from_apply\n            other: 7\n";
        let root = crate::testing::project(
            "t167-edge",
            "[defaults]\n",
            &[("inc.yml", "- debug: {msg: \"{{ applied }}\"}\n"), ("play.yml", PLAY)],
        );
        let mut idx = ReverseIndex::default();
        idx.replace(edges(&root, "play.yml"));

        assert_eq!(
            applied(&idx, &root, "inc.yml", PLAY),
            vec![("applied".to_string(), "from_apply".to_string()), ("other".to_string(), "7".to_string())],
            "both entries ride the edge, each pointing at its own value"
        );
        // Control: an include with no `apply:` carries none, so the field cannot be junk
        // that happens to be present everywhere.
        let root = crate::testing::project(
            "t167-edge-none",
            "[defaults]\n",
            &[("inc.yml", "- debug: {msg: hi}\n"), ("play.yml", "- hosts: all\n  tasks:\n    - include_tasks: inc.yml\n")],
        );
        let mut idx = ReverseIndex::default();
        idx.replace(edges(&root, "play.yml"));
        assert_eq!(idx.inbound(&canon(&root).join("inc.yml")).len(), 1, "the edge is there");
        assert!(applied(&idx, &root, "inc.yml", "").is_empty(), "and carries no apply vars");
    }

    /// A templated include reaches several files and every one of them gets the vars — the
    /// same overcount rule the edges themselves follow.
    #[test]
    fn a_templated_include_applies_its_vars_to_every_candidate() {
        const PLAY: &str = "- hosts: all\n  tasks:\n    - include_tasks:\n        file: \"check-{{ env }}.yml\"\n        apply:\n          vars:\n            applied: from_apply\n";
        let root = crate::testing::project(
            "t167-edge-templated",
            "[defaults]\n",
            &[
                ("check-prod.yml", "- debug: {msg: p}\n"),
                ("check-dev.yml", "- debug: {msg: d}\n"),
                ("play.yml", PLAY),
            ],
        );
        let mut idx = ReverseIndex::default();
        idx.replace(edges(&root, "play.yml"));
        for f in ["check-prod.yml", "check-dev.yml"] {
            assert_eq!(
                applied(&idx, &root, f, PLAY),
                vec![("applied".to_string(), "from_apply".to_string())],
                "{f}"
            );
        }
    }

    fn lines_into<'a>(idx: &'a ReverseIndex, root: &Path, rel: &str) -> Vec<(String, u32, ReferenceKind, bool)> {
        let root = canon(root);
        idx.inbound(&root.join(rel))
            .iter()
            .map(|e| {
                let rel = e.source.strip_prefix(&root).unwrap_or(&e.source);
                (crate::posix_display(rel), e.line, e.kind, e.templated)
            })
            .collect()
    }

    #[test]
    fn a_literal_include_is_one_edge_at_its_line() {
        let root = crate::testing::project(
            "t020-literal",
            "[defaults]\n",
            &[
                ("tasks/shared.yml", "- debug: {msg: hi}\n"),
                ("play.yml", "- hosts: all\n  tasks:\n    - import_tasks: tasks/shared.yml\n    - include_tasks: tasks/shared.yml\n"),
            ],
        );
        let mut idx = ReverseIndex::default();
        idx.replace(edges(&root, "play.yml"));
        assert_eq!(
            lines_into(&idx, &root, "tasks/shared.yml"),
            vec![
                ("play.yml".into(), 2, ReferenceKind::ImportTasks, false),
                ("play.yml".into(), 3, ReferenceKind::IncludeTasks, false),
            ],
            "two lines, two edges — a file pair is not the unit"
        );
        // Control: a file nothing points at has no inbound edges.
        assert!(idx.inbound(&root.join("play.yml")).is_empty());
    }

    /// The case that broke a grep-based estimate: `validate-{{ op }}.yml` reaches every
    /// `validate-*.yml`, and each must count as reached.
    #[test]
    fn a_templated_include_contributes_an_edge_per_candidate() {
        let root = crate::testing::project(
            "t020-templated",
            "[defaults]\n",
            &[
                ("tasks/validate-expose.yml", "- debug: {msg: a}\n"),
                ("tasks/validate-attach.yml", "- debug: {msg: b}\n"),
                ("tasks/other.yml", "- debug: {msg: c}\n"),
                ("play.yml", "- hosts: all\n  tasks:\n    - include_tasks: \"tasks/validate-{{ op }}.yml\"\n"),
            ],
        );
        let mut idx = ReverseIndex::default();
        idx.replace(edges(&root, "play.yml"));
        assert_eq!(
            lines_into(&idx, &root, "tasks/validate-expose.yml"),
            vec![("play.yml".into(), 2, ReferenceKind::IncludeTasks, true)]
        );
        assert_eq!(
            lines_into(&idx, &root, "tasks/validate-attach.yml"),
            vec![("play.yml".into(), 2, ReferenceKind::IncludeTasks, true)]
        );
        // Control: the glob is anchored, so a file outside the pattern gets nothing.
        assert!(idx.inbound(&root.join("tasks/other.yml")).is_empty());
        assert_eq!(idx.len(), 2);
    }

    #[test]
    fn replacing_a_source_drops_its_old_edges_and_only_those() {
        let root = crate::testing::project(
            "t020-replace",
            "[defaults]\n",
            &[
                ("tasks/a.yml", "- debug: {msg: a}\n"),
                ("tasks/b.yml", "- debug: {msg: b}\n"),
                ("one.yml", "- hosts: all\n  tasks:\n    - import_tasks: tasks/a.yml\n"),
                ("two.yml", "- hosts: all\n  tasks:\n    - import_tasks: tasks/a.yml\n"),
            ],
        );
        let mut idx = ReverseIndex::default();
        idx.replace(edges(&root, "one.yml"));
        idx.replace(edges(&root, "two.yml"));
        assert_eq!(idx.inbound(&root.join("tasks/a.yml")).len(), 2);

        // one.yml now points at b instead of a.
        std::fs::write(root.join("one.yml"), "- hosts: all\n  tasks:\n    - import_tasks: tasks/b.yml\n").unwrap();
        idx.replace(edges(&root, "one.yml"));
        assert_eq!(lines_into(&idx, &root, "tasks/a.yml"), vec![("two.yml".into(), 2, ReferenceKind::ImportTasks, false)]);
        assert_eq!(lines_into(&idx, &root, "tasks/b.yml"), vec![("one.yml".into(), 2, ReferenceKind::ImportTasks, false)]);

        // And an empty replacement — the file stopped parsing — removes it entirely.
        idx.replace(SourceEdges { source: canon(&root.join("one.yml")), edges: Vec::new() });
        assert!(idx.inbound(&root.join("tasks/b.yml")).is_empty());
        assert_eq!(idx.sources().count(), 1);

        idx.retain_sources(|_| false);
        assert!(idx.is_empty());
    }

    /// Two spellings of one target are one key.
    #[test]
    fn keys_collapse_spellings_of_one_path() {
        let root = crate::testing::project(
            "t020-spelling",
            "[defaults]\n",
            &[
                ("tasks/a.yml", "- debug: {msg: a}\n"),
                ("play.yml", "- hosts: all\n  tasks:\n    - import_tasks: tasks/a.yml\n    - import_tasks: ./tasks/../tasks/a.yml\n"),
            ],
        );
        let mut idx = ReverseIndex::default();
        idx.replace(edges(&root, "play.yml"));
        assert_eq!(idx.inbound(&root.join("tasks/a.yml")).len(), 2, "{:?}", idx.targets());
        assert_eq!(idx.targets().len(), 1);
    }
}
