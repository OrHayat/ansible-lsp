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
    /// The value carried a Jinja expression, so this is one of the candidates that line may
    /// reach rather than the one file it names.
    pub templated: bool,
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
        for target in &res.targets {
            edges.push((
                key(target),
                Edge { source: shared.clone(), span: r.span, line, kind: r.kind, templated: r.templated },
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
