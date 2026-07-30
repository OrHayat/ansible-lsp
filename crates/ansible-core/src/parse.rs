//! YAML -> byte-span AST. The only module that touches `saphyr`.
//!
//! saphyr's `Marker` uses CHARACTER offsets, not bytes — invisible in ASCII, wrong on
//! any line with non-ASCII (this repo has em dashes in task names). Converting here,
//! once, lets every other module assume bytes and slice `&text[span]` safely.

use saphyr::{LoadableYamlNode, MarkedYaml, YamlData};

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
    pub fn get(&self, key: &str) -> Option<&Node> {
        match self {
            Node::Mapping { entries, .. } => entries
                .iter()
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

/// A parsed source file: the text plus a line index, so byte<->line/col is cheap.
pub struct Document {
    pub text: String,
    /// Byte offset at which each line begins.
    line_starts: Vec<usize>,
}

impl Document {
    pub fn new(text: String) -> Self {
        let mut line_starts = vec![0];
        line_starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Self { text, line_starts }
    }

    /// saphyr `Marker` -> byte offset. `line` is 1-based, `col` a 0-based CHAR column.
    fn marker_to_byte(&self, line: usize, char_col: usize) -> usize {
        let idx = line.saturating_sub(1);
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
        match line_text.char_indices().nth(char_col) {
            Some((b, _)) => start + b,
            // Column past end of line (e.g. a span ending at the newline): clamp.
            None => end,
        }
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

    /// `None` = not valid YAML 1.2. Not an error: strict YAML rejects files the
    /// PyYAML Ansible uses accepts, so callers must degrade to "no references" rather
    /// than reporting anything broken.
    pub fn parse(&self) -> Option<Vec<Node>> {
        let docs = MarkedYaml::load_from_str(&self.text).ok()?;
        Some(docs.iter().map(|d| self.convert(d)).collect())
    }

    fn convert(&self, n: &MarkedYaml) -> Node {
        let mut span = Span {
            start: self.marker_to_byte(n.span.start.line(), n.span.start.col()),
            end: self.marker_to_byte(n.span.end.line(), n.span.end.col()),
        };
        // A quoted scalar's span covers its quotes; ranges should mark the value.
        let text = span.slice(&self.text);
        if text.len() >= 2 {
            let b = text.as_bytes();
            if (b[0] == b'"' || b[0] == b'\'') && b[0] == b[text.len() - 1] {
                span.start += 1;
                span.end -= 1;
            }
        }
        match &n.data {
            YamlData::Value(v) => Node::Scalar {
                value: v
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("{v:?}")),
                span,
            },
            YamlData::Sequence(items) => Node::Sequence {
                items: items.iter().map(|c| self.convert(c)).collect(),
                span,
            },
            YamlData::Mapping(map) => Node::Mapping {
                entries: map
                    .iter()
                    .map(|(k, v)| (self.convert(k), self.convert(v)))
                    .collect(),
                span,
            },
            _ => Node::Other { span },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Non-ASCII before the target shifts saphyr's char marker off the byte offset.
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

    #[test]
    fn comments_are_not_references() {
        let src = "# - include_tasks: commented_out.yml\n- include_tasks: real.yml\n";
        let doc = Document::new(src.to_string());
        let nodes = doc.parse().unwrap();
        assert_eq!(nodes[0].items().len(), 1);
    }
}
