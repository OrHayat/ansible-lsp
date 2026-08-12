//! T-168: a YAML complex key parses clean but is fatal to Ansible's loader.
//!
//! YAML permits non-scalar mapping keys; Python dicts do not. Our libyaml accepts every
//! spelling of one — measured on all five forms below — while Ansible refuses each,
//! either in its scanner (`[a, b]: x`, `{a: 1}: x` inline) or constructing the dict
//! (`? [a, b]` explicit form, `While constructing a mapping found unhashable key`). The
//! overwhelmingly common way to write one is an unquoted template as a key:
//!
//! ```yaml
//! set_fact:
//!   {{ result_name }}: true    # two flow mappings, not a template
//! ```
//!
//! which is why upstream's own wrapper says "This may be an issue with missing quotes
//! around a template block" whenever the offending line carries `{{`. The message here
//! follows that same line-level split: template-flavoured with the quoting fix when the
//! offending line carries braces, the plain complex-key truth otherwise.
//!
//! Every document kind is walked: the constructor runs for playbooks, task files and
//! vars files alike — measured fatal in a playbook and via `vars_files:`.

use crate::parse::{Node, Span};

/// Rule id, for `# noqa: complex-key` and for display.
pub const RULE_ID: &str = "complex-key";

#[derive(Debug, Clone)]
pub struct Problem {
    /// The non-scalar key.
    pub span: Span,
    pub message: String,
    pub rule: &'static str,
}

const TEMPLATED: &str = "an unquoted template is read as YAML flow mappings, not a \
                         template — Ansible refuses to load the file. Quote it: \
                         \"{{ ... }}\".";
const PLAIN: &str = "a mapping or sequence cannot be a mapping key — Ansible refuses to \
                     load the file.";

/// Every complex key in the document, at any depth and in any document kind. `src` is the
/// document text, used only to tell a mistyped template from a deliberate complex key.
pub fn problems(nodes: &[Node], src: &str) -> Vec<Problem> {
    let mut out = Vec::new();
    for n in nodes {
        walk(n, src, &mut out);
    }
    out
}

fn walk(node: &Node, src: &str, out: &mut Vec<Problem>) {
    match node {
        Node::Mapping { entries, .. } => {
            for (k, v) in entries {
                if matches!(k, Node::Mapping { .. } | Node::Sequence { .. }) {
                    // The braces test reads the whole LINE, not the key's own text:
                    // `msg: {{ x }}` puts the complex key at the brace-less inner
                    // `{ x }`, yet the line carries the braces and quoting is the fix.
                    // Upstream's own hint uses the same line-level split.
                    let templated = line_of(src, k.span().start).contains("{{");
                    out.push(Problem {
                        span: k.span(),
                        message: if templated { TEMPLATED } else { PLAIN }.into(),
                        rule: RULE_ID,
                    });
                }
                // The key itself is not recursed into: a non-scalar key was flagged
                // above and its interior is part of the same fault, while every other
                // key kind (scalar, null, alias — the last is T-160's blind spot) has
                // no interior to walk.
                walk(v, src, out);
            }
        }
        Node::Sequence { items, .. } => items.iter().for_each(|i| walk(i, src, out)),
        Node::Scalar { .. } | Node::Null { .. } | Node::Other { .. } => {}
    }
}

/// The full source line containing byte `at`.
fn line_of(src: &str, at: usize) -> &str {
    let start = src[..at].rfind('\n').map_or(0, |i| i + 1);
    let end = src[at..].find('\n').map_or(src.len(), |i| at + i);
    &src[start..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::Document;

    fn check(src: &str) -> Vec<String> {
        let nodes = Document::new(src.to_string()).parse().expect("valid yaml");
        problems(&nodes, src).into_iter().map(|p| p.message).collect()
    }

    /// The symptom spelling: an unquoted template as a `set_fact` key. Our libyaml parses
    /// it as nested flow mappings; Ansible refuses the file with the missing-quotes hint.
    #[test]
    fn an_unquoted_template_key_is_an_error_with_the_quoting_fix() {
        let got = check(
            "- hosts: web\n  tasks:\n    - ansible.builtin.set_fact:\n        {{ result_name }}: true\n",
        );
        assert_eq!(got, [TEMPLATED.to_string()]);
    }

    /// The same fault in a vars file — the constructor runs for every document kind, and
    /// this exact file was measured fatal through `vars_files:`.
    #[test]
    fn a_template_key_in_a_vars_file_is_an_error() {
        assert_eq!(check("{{ name }}: value\n"), [TEMPLATED.to_string()]);
    }

    /// The deliberate spellings, without braces: inline and explicit-`?` flow collections
    /// as keys. All parse here and none load in Ansible — the explicit form is the one
    /// that reaches the constructor's own `found unhashable key`.
    #[test]
    fn sequence_and_mapping_keys_are_errors_in_every_spelling() {
        for src in [
            "- hosts: web\n  tasks:\n    - debug:\n        [a, b]: true\n",
            "- hosts: web\n  tasks:\n    - debug:\n        ? [a, b]\n        : true\n",
            "- hosts: web\n  tasks:\n    - debug:\n        {a: 1}: true\n",
        ] {
            assert_eq!(check(src), [PLAIN.to_string()], "in: {src}");
        }
    }

    /// The control: quoted, the key is an ordinary string — valid, and in module args it
    /// even templates (T-103's measured non-case).
    #[test]
    fn a_quoted_template_key_is_clean() {
        assert!(check(
            "- hosts: web\n  tasks:\n    - ansible.builtin.set_fact:\n        \"{{ result_name }}\": true\n"
        )
        .is_empty());
        assert!(check("\"{{ name }}\": value\n").is_empty());
    }

    /// Scalar and null keys are hashable and load fine; an alias key is T-160's blind
    /// spot and stays a miss rather than a guess.
    #[test]
    fn hashable_and_alias_keys_are_not_flagged() {
        assert!(check("- hosts: web\n  tasks:\n    - debug: {msg: x}\n").is_empty());
        assert!(check("~: v\n42: w\n").is_empty());
        assert!(check("anchor: &a x\n*a : v\n").is_empty());
    }

    /// The value-position spelling: `msg: {{ x }}` parses as a mapping whose inner key
    /// is the brace-less `{ x }`, so a key-text check misses the braces — but the LINE
    /// carries them, which is the same split upstream's own hint uses. The author's fix
    /// is quoting either way.
    #[test]
    fn a_template_in_value_position_gets_the_quoting_fix() {
        assert_eq!(
            check("- hosts: web\n  tasks:\n    - debug:\n        msg: {{ x }}\n"),
            [TEMPLATED.to_string()]
        );
    }

    /// The problem is anchored on the key, wherever it nests.
    #[test]
    fn anchors_on_the_key() {
        let src = "- hosts: web\n  vars:\n    outer:\n      inner:\n        {{ x }}: 1\n";
        let nodes = Document::new(src.to_string()).parse().expect("valid yaml");
        let got = problems(&nodes, src);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].span.slice(src), "{{ x }}");
    }
}
