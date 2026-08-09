//! Byte-span AST types plus the source/line index. The actual YAML parsing lives in
//! [`crate::parse_libyaml`] (libyaml, lenient like Ansible); this module owns the [`Node`]
//! tree, [`Span`], and byte<->line/UTF-16 conversion every other module builds on.

/// Byte offsets. Always bytes — never chars, never UTF-16.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn slice<'a>(&self, text: &'a str) -> &'a str {
        text.get(self.start..self.end).unwrap_or("")
    }
}

pub use crate::parse_libyaml::DuplicateKey;

/// Ansible's own test is "did `json.loads` succeed", and no heuristic reproduces it — a
/// trailing comma is invalid JSON and valid YAML, and flips the answer on that alone. So run
/// a real JSON parse.
///
/// Gated on the first non-space byte, which keeps the common case to one comparison: a JSON
/// document that is not an object or array holds no mapping, so it cannot carry a duplicate
/// key — the only thing this distinction currently decides.
///
/// Known gap: CPython's `json` accepts bare `NaN`/`Infinity`/`-Infinity` and `serde_json`
/// rejects them, so such a file is JSON to Ansible and YAML to us.
fn detect_loader(text: &str) -> Loader {
    let t = text.trim_start();
    if !t.starts_with('{') && !t.starts_with('[') {
        return Loader::Yaml;
    }
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(_) => Loader::Json,
        Err(_) => Loader::Yaml,
    }
}

#[derive(Debug, Clone)]
pub enum Node {
    Scalar {
        value: String,
        span: Span,
    },
    Sequence {
        items: Vec<Node>,
        span: Span,
    },
    Mapping {
        entries: Vec<(Node, Node)>,
        span: Span,
    },
    /// YAML null — a key written with no value. Distinct from `Scalar { value: "" }`, which
    /// is an explicitly empty *string*: Ansible reads the first as absence and the second as
    /// a value, and for a conditional that is the difference between fine and fatal (T-117).
    /// The span is empty and sits where the value would have been.
    Null {
        span: Span,
    },
    Other {
        span: Span,
    },
}

impl Node {
    pub fn span(&self) -> Span {
        match self {
            Node::Scalar { span, .. }
            | Node::Sequence { span, .. }
            | Node::Mapping { span, .. }
            | Node::Null { span }
            | Node::Other { span } => *span,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Node::Scalar { value, .. } => Some(value),
            _ => None,
        }
    }

    /// Key lookup. Makes `include_role: { name: x, tasks_from: y }` work: the pair
    /// are siblings in the *same* mapping node, not nearby lines.
    ///
    /// Searches from the end, because a duplicate key is legal YAML and Ansible's
    /// constructor assigns it twice — last one wins, first value discarded before any play
    /// sees it. Taking the first would analyse the dead value: a duplicated `include_tasks:`
    /// would link the wrong file, a duplicated `when:` would explain the wrong condition.
    pub fn get(&self, key: &str) -> Option<&Node> {
        match self {
            Node::Mapping { entries, .. } => entries
                .iter()
                .rev()
                .find(|(k, _)| k.as_str() == Some(key))
                .map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn entries(&self) -> &[(Node, Node)] {
        match self {
            Node::Mapping { entries, .. } => entries,
            _ => &[],
        }
    }

    pub fn items(&self) -> &[Node] {
        match self {
            Node::Sequence { items, .. } => items,
            _ => &[],
        }
    }
}

/// Which parser Ansible reads a file's *contents* with. `from_yaml` tries `json.loads`
/// before YAML (`parsing/utils/yaml.py:41`), so contents that parse as JSON are read as JSON
/// whatever the file is named — and never reach the YAML constructor, where the duplicate-key
/// check lives. Independent of the `.yml`/`.yaml`/`.json` *extension* order, which decides
/// which file to open, not how to read it.
///
/// Per file, not per directory — which is why this lives on [`Document`] and not on
/// `FileContext`, whose cache keys on the directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Loader {
    #[default]
    Yaml,
    Json,
}

/// A parsed source file: the text plus a line index, so byte<->line/col is cheap.
pub struct Document {
    pub text: String,
    /// Byte offset at which each line begins.
    line_starts: Vec<usize>,
    loader: Loader,
}

impl Document {
    pub fn new(text: String) -> Self {
        let mut line_starts = vec![0];
        line_starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        let loader = detect_loader(&text);
        Self { text, line_starts, loader }
    }

    /// Which parser Ansible would read this file's contents with. See [`Loader`].
    pub fn loader(&self) -> Loader {
        self.loader
    }

    /// 0-based line and 0-based UTF-16 column, for the LSP boundary.
    pub fn byte_to_lsp(&self, byte: usize) -> (u32, u32) {
        let line = self
            .line_starts
            .partition_point(|&start| start <= byte)
            .saturating_sub(1);
        let line_start = self.line_starts.get(line).copied().unwrap_or(0);
        let prefix = self.text.get(line_start..byte).unwrap_or("");
        (line as u32, prefix.encode_utf16().count() as u32)
    }

    /// 1-based line number of a byte offset, via the precomputed line index.
    pub fn line_of(&self, byte: usize) -> usize {
        self.line_starts.partition_point(|&start| start <= byte)
    }

    /// Inverse of [`Self::byte_to_lsp`], for turning a cursor position into an offset.
    pub fn lsp_to_byte(&self, line: u32, utf16_col: u32) -> usize {
        let idx = line as usize;
        let start = self
            .line_starts
            .get(idx)
            .copied()
            .unwrap_or(self.text.len());
        let end = self
            .line_starts
            .get(idx + 1)
            .copied()
            .unwrap_or(self.text.len());
        let line_text = self.text.get(start..end).unwrap_or("");
        let mut utf16 = 0u32;
        for (b, c) in line_text.char_indices() {
            if utf16 >= utf16_col {
                return start + b;
            }
            utf16 += c.len_utf16() as u32;
        }
        end
    }

    /// Is the diagnostic `rule` at `byte` silenced by a `# noqa` comment?
    ///
    /// Follows ansible-lint: `# noqa: rule-a rule-b` silences only those rules, a bare
    /// `# noqa` silences everything. Rule scoping matters — this repo already carries
    /// `# noqa: command-instead-of-module` for ansible-lint, and a substring match
    /// would let those lines silently disable our checks too.
    ///
    /// Accepted on the reference's own line or the line above. Comments are absent
    /// from the AST, so this reads raw source.
    pub fn is_suppressed(&self, byte: usize, rule: &str) -> bool {
        let (line, _) = self.byte_to_lsp(byte);
        let idx = line as usize;
        self.line_suppresses(idx, rule) || (idx > 0 && self.line_suppresses(idx - 1, rule))
    }

    fn line_suppresses(&self, idx: usize, rule: &str) -> bool {
        let Some(&start) = self.line_starts.get(idx) else {
            return false;
        };
        let end = self
            .line_starts
            .get(idx + 1)
            .copied()
            .unwrap_or(self.text.len());
        let Some(line) = self.text.get(start..end) else {
            return false;
        };
        let Some(at) = line.find("# noqa") else {
            return false;
        };
        let rest = line[at + "# noqa".len()..].trim_end();
        match rest.strip_prefix(':') {
            // Scoped: only the rules named.
            Some(ids) => ids
                .split([' ', ',', '\t'])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .any(|id| id == rule),
            // Bare `# noqa` silences everything on the line.
            None => true,
        }
    }

    /// `None` = not valid YAML even to Ansible. Parsing is delegated to
    /// [`crate::parse_libyaml`], which follows libyaml's leniency, so files that are valid
    /// to Ansible but not to strict YAML 1.2 (under-indented multi-line quoted scalars)
    /// analyse like any other.
    pub fn parse(&self) -> Option<Vec<Node>> {
        crate::parse_libyaml::parse_lenient(&self.text)
    }

    /// Where [`Self::parse`] failed, as a byte span, or `None` if it parses. Because parsing
    /// now matches Ansible, a failure here means the file is broken *for Ansible too* — so a
    /// caller may say so more firmly than the old strict-1.2 "may still be valid" hedge.
    pub fn parse_error(&self) -> Option<Span> {
        crate::parse_libyaml::parse_lenient_error(&self.text)
    }

    /// Every repeated mapping key, one entry per later occurrence. See [`DuplicateKey`].
    pub fn duplicate_keys(&self) -> Vec<DuplicateKey> {
        crate::parse_libyaml::duplicate_keys(&self.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Which parser Ansible reads the contents with — "did `json.loads` succeed", not the
    /// extension and not a shape heuristic. Every case here is live-verified on 2.21.2.
    #[test]
    fn loader_follows_a_real_json_parse() {
        let of = |s: &str| Document::new(s.to_string()).loader();

        // Ordinary YAML, including flow style that merely looks JSON-ish.
        assert_eq!(of("- hosts: localhost\n"), Loader::Yaml);
        assert_eq!(of("{a: 1, b: 2}\n"), Loader::Yaml, "unquoted keys are not JSON");
        assert_eq!(of(""), Loader::Yaml);

        // Strict JSON, whatever the file is called.
        assert_eq!(of("[{\"p\": 80, \"p\": 8080}]\n"), Loader::Json);
        assert_eq!(of("  \n\t{\"a\": 1}\n"), Loader::Json, "leading space is fine");

        // The pair that proves a heuristic can't do this: one comma apart, and Ansible
        // warns about the second and not the first.
        assert_eq!(of("[{\"a\": 1, \"a\": 2}]"), Loader::Json);
        assert_eq!(of("[{\"a\": 1, \"a\": 2},]"), Loader::Yaml, "trailing comma → YAML");
    }

    /// A duplicate mapping key is legal YAML and Ansible keeps the LAST value (verified
    /// against ansible-core 2.21.2, which warns and runs with `8080`). Both entries reach
    /// the tree, so `get` has to pick the one the play actually sees.
    #[test]
    fn duplicate_key_resolves_to_the_last_value_like_ansible() {
        let src = "vars:\n  http_port: 80\n  http_port: 8080\n";
        let doc = Document::new(src.to_string());
        let nodes = doc.parse().expect("valid yaml");
        let vars = nodes[0].get("vars").expect("vars");

        assert_eq!(vars.entries().len(), 2, "both entries stay in the tree");
        assert_eq!(vars.get("http_port").and_then(Node::as_str), Some("8080"));
    }

    /// The span must follow the value, or the diagnostic underlines the dead line.
    #[test]
    fn duplicate_key_span_points_at_the_winning_occurrence() {
        let src = "- include_tasks: dead.yml\n  include_tasks: live.yml\n";
        let doc = Document::new(src.to_string());
        let nodes = doc.parse().expect("valid yaml");
        let v = nodes[0].items()[0].get("include_tasks").expect("include_tasks");

        assert_eq!(v.span().slice(src), "live.yml");
    }

    /// Non-ASCII before the target must not shift the target's byte offset — the classic
    /// char-vs-byte marker trap.
    #[test]
    fn spans_are_byte_accurate_past_non_ascii() {
        let src = "---\n\
                   - name: \"title with — an em dash\"\n  \
                   ansible.builtin.include_role: { name: cib-batch, tasks_from: begin }\n";
        let doc = Document::new(src.to_string());
        let nodes = doc.parse().expect("valid yaml");

        let task = &nodes[0].items()[0];
        let inc = task
            .get("ansible.builtin.include_role")
            .expect("include_role");
        let name = inc.get("name").expect("name");
        let from = inc.get("tasks_from").expect("tasks_from");

        // The whole point: slicing the source by our span yields exactly the scalar.
        assert_eq!(name.span().slice(src), "cib-batch");
        assert_eq!(from.span().slice(src), "begin");
    }

    #[test]
    fn emoji_and_utf16_roundtrip() {
        // 🚀 is 4 UTF-8 bytes but 2 UTF-16 units.
        let src = "- name: \"🚀 launch\"\n  include_tasks: go.yml\n";
        let doc = Document::new(src.to_string());
        let nodes = doc.parse().expect("valid yaml");
        let target = nodes[0].items()[0].get("include_tasks").unwrap();
        assert_eq!(target.span().slice(src), "go.yml");

        let (line, col) = doc.byte_to_lsp(target.span().start);
        assert_eq!(line, 1);
        assert_eq!(doc.lsp_to_byte(line, col), target.span().start);

        // The emoji line: 2 UTF-16 units for one 4-byte char.
        let quote = src.find("🚀").unwrap();
        let (l, c) = doc.byte_to_lsp(quote);
        assert_eq!((l, c), (0, 9));
        assert_eq!(doc.lsp_to_byte(l, c), quote);
    }

    #[test]
    fn parse_error_points_at_the_break() {
        // Unquoted ": " inside a value — the single most common accidental YAML break.
        let doc = Document::new("---\n- name: Block form with a file: parameter\n".to_string());
        assert!(doc.parse().is_none(), "this should not parse");
        let span = doc.parse_error().expect("a failed parse must report where");
        let (line, _) = doc.byte_to_lsp(span.start);
        assert_eq!(line, 1, "the break is on the second line");
        assert!(span.end > span.start, "range must not be empty");
    }

    #[test]
    fn parse_error_is_none_when_it_parses() {
        let doc = Document::new("- name: fine\n  include_tasks: go.yml\n".to_string());
        assert!(doc.parse_error().is_none());
    }

    #[test]
    fn quoted_scalar_span_excludes_the_quotes() {
        let src = "- include_tasks: \"{{ proto }}/x.yml\"\n";
        let doc = Document::new(src.to_string());
        let nodes = doc.parse().unwrap();
        let v = nodes[0].items()[0].get("include_tasks").unwrap();
        assert_eq!(v.span().slice(src), "{{ proto }}/x.yml");
    }

    #[test]
    fn bare_noqa_suppresses_on_the_line_and_the_line_above() {
        let src = "- include_tasks: a.yml\n\
                   - include_tasks: b.yml  # noqa\n\
                   # noqa\n\
                   - include_tasks: c.yml\n";
        let doc = Document::new(src.to_string());
        let at = |needle: &str| src.find(needle).unwrap();
        assert!(!doc.is_suppressed(at("a.yml"), "missing-file"));
        assert!(doc.is_suppressed(at("b.yml"), "missing-file"));
        assert!(doc.is_suppressed(at("c.yml"), "missing-file"));
    }

    /// This repo carries `# noqa: command-instead-of-module` for ansible-lint. A
    /// substring match would let those lines disable our checks by accident.
    #[test]
    fn another_tools_noqa_does_not_silence_ours() {
        let src = "- include_tasks: a.yml  # noqa: command-instead-of-module\n\
                   - include_tasks: b.yml  # noqa: missing-file\n\
                   - include_tasks: c.yml  # noqa: yaml[line-length] missing-file\n";
        let doc = Document::new(src.to_string());
        let at = |needle: &str| src.find(needle).unwrap();
        assert!(!doc.is_suppressed(at("a.yml"), "missing-file"));
        assert!(doc.is_suppressed(at("b.yml"), "missing-file"));
        assert!(doc.is_suppressed(at("c.yml"), "missing-file"));
        assert!(!doc.is_suppressed(at("c.yml"), "templated-import"));
    }

    #[test]
    fn unparseable_yields_none_not_panic() {
        let doc = Document::new("- name: \"unterminated\n  bad: [".to_string());
        assert!(doc.parse().is_none());
    }

    /// `when: false` is a YAML boolean, and rendering it with Rust's `Debug` gave
    /// "Boolean(false)" — so nothing downstream could see it as falsy.
    #[test]
    fn non_string_scalars_render_as_yaml_text() {
        let src = "- when: false\n  other: 42\n  legacy: yes\n";
        let doc = Document::new(src.to_string());
        let nodes = doc.parse().unwrap();
        let m = &nodes[0].items()[0];
        assert_eq!(m.get("when").unwrap().as_str(), Some("false"));
        assert_eq!(m.get("other").unwrap().as_str(), Some("42"));
        // `yes` is a plain STRING in YAML 1.2 — it was only a boolean in 1.1, which is
        // what PyYAML and therefore Ansible still use. Another face of the laxity
        // difference already pinned by `unparseable_yields_none_not_panic`. Consumers
        // that care must treat "yes"/"no" as booleans themselves; `condition::is_truthy`
        // does.
        assert_eq!(m.get("legacy").unwrap().as_str(), Some("yes"));
    }

    #[test]
    fn comments_are_not_references() {
        let src = "# - include_tasks: commented_out.yml\n- include_tasks: real.yml\n";
        let doc = Document::new(src.to_string());
        let nodes = doc.parse().unwrap();
        assert_eq!(nodes[0].items().len(), 1);
    }
}
