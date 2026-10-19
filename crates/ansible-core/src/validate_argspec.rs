//! A play's `validate_argspec:` and the `<playbook>.meta{ext}` file it reads (T-153).
//!
//! `Play._post_validate_validate_argspec` (`playbook/play.py:396-450`) picks a spec name — the
//! string written, or the play's `name:` for `true` — and looks it up under `argument_specs`
//! in the first of `<playbook>.meta.yml`, `.meta.yaml`, `.meta.json`, `.meta` that exists. Any
//! miss is an `AnsibleParserError` at load; measured on 2.21.2, every one of them also fails
//! `--syntax-check`. One answer per play, read by the diagnostic and by go-to-definition.

use std::path::{Path, PathBuf};

use crate::ast::{self, Ast};
use crate::cache::ScanCache;
use crate::fs::Fs;
use crate::parse::{Node, Span};

pub const RULE_ID: &str = "validate-argspec";

/// What a play's `validate_argspec:` comes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The spec the play names, at its key in the meta file.
    Found { meta: PathBuf, entry: Span },
    /// Ansible's own message for the load error.
    Fault(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    /// The `validate_argspec:` value, where the diagnostic and the jump both anchor.
    pub value: Span,
    pub outcome: Outcome,
}

/// `YAML_FILENAME_EXTENSIONS`' default, then no extension (`play.py:455-461`). The setting is
/// configurable and not read here, the same as everywhere else that uses this list.
const EXTENSIONS: &[&str] = &[".yml", ".yaml", ".json", ""];

/// One [`Site`] per play in `playbook` that turns spec validation on. A templated value or
/// play name, and a meta file that does not parse, give no site: the first is decided at run
/// time and the second fails with a different error, and neither is this rule's to guess.
pub fn sites(nodes: &[Node], text: &str, playbook: &Path, cache: &ScanCache) -> Vec<Site> {
    if !matches!(ast::build(nodes), Ast::Playbook(_)) {
        return Vec::new();
    }
    nodes.iter().flat_map(Node::items).filter_map(|play| site(play, text, playbook, cache)).collect()
}

fn site(play: &Node, text: &str, playbook: &Path, cache: &ScanCache) -> Option<Site> {
    let value = play.get("validate_argspec")?;
    let Node::Scalar { value: raw, span, .. } = value else { return None };
    if raw.contains("{{") || raw.contains("{%") {
        return None;
    }
    // A plain `true`/`yes` is a boolean and a quoted one is a spec *named* `true` — measured:
    // `validate_argspec: "true"` is "No argument spec named 'true'". The parse tree keeps no
    // scalar style (T-162), so the quotes are read off the source.
    let quoted = span.start > 0 && matches!(text.as_bytes().get(span.start - 1), Some(b'"' | b'\''));
    let name = match (quoted, yaml11_bool(raw)) {
        (false, Some(false)) => return None,
        (false, Some(true)) => match play.get("name").and_then(Node::as_str) {
            Some(n) if n.contains("{{") || n.contains("{%") => return None,
            Some(n) if !n.is_empty() => Some(n.to_string()),
            _ => None,
        },
        _ if raw.is_empty() => return None,
        _ => Some(raw.clone()),
    };
    let candidates = candidates(playbook);
    let meta = candidates.iter().find(|p| cache.is_file(p)).cloned();
    let fault = |m: String| Some(Site { value: *span, outcome: Outcome::Fault(format!("Error processing keyword 'validate_argspec': {m}")) });

    let mut missing = Vec::new();
    if name.is_none() {
        missing.push(
            "A play name is required when validate_argspec is True. Alternatively, set \
             validate_argspec to the name of an argument spec."
                .to_string(),
        );
    }
    if meta.is_none() {
        let considered: Vec<String> = candidates.iter().map(|p| file_name(p)).collect();
        missing.push(format!("A playbook meta file is required. Considered: {}", considered.join(", ")));
    }
    if !missing.is_empty() {
        return fault(missing.join(" "));
    }
    let (name, meta) = (name?, meta?);
    let src = cache.source(&meta)?;
    let doc = src.nodes.as_deref()?.first()?;
    let entry = doc.get("argument_specs").and_then(|s| {
        s.entries().iter().rev().find(|(k, _)| k.as_str() == Some(name.as_str()))
    });
    match entry {
        Some((key, spec)) if matches!(spec.get("options"), Some(Node::Mapping { .. })) => {
            Some(Site { value: *span, outcome: Outcome::Found { meta, entry: key.span() } })
        }
        _ => fault(format!(
            "No argument spec named '{name}' in {}. Minimally expected: \
             argument_specs: {{{name}: {{options: {{}}}}}}",
            file_name(&meta)
        )),
    }
}

/// `site.yml` → `site.meta.yml`, …: the playbook's own extension comes off first when it is
/// one of the list, otherwise the whole name is the stem (`play.py:455-461`).
fn candidates(playbook: &Path) -> Vec<PathBuf> {
    let name = playbook.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let stem = EXTENSIONS
        .iter()
        .filter(|e| !e.is_empty())
        .find_map(|e| name.strip_suffix(e))
        .unwrap_or(name);
    EXTENSIONS.iter().map(|e| playbook.with_file_name(format!("{stem}.meta{e}"))).collect()
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The YAML 1.1 booleans PyYAML resolves a plain scalar to. Measured: plain `yes` turned
/// validation on with the play's name.
fn yaml11_bool(s: &str) -> Option<bool> {
    match s {
        "true" | "True" | "TRUE" | "yes" | "Yes" | "YES" | "on" | "On" | "ON" => Some(true),
        "false" | "False" | "FALSE" | "no" | "No" | "NO" | "off" | "Off" | "OFF" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::Document;

    const SPEC: &str = "argument_specs:\n  myspec:\n    options: {}\n  named play:\n    options: {}\n";

    /// The play written into `site.yml` beside each `(meta file, contents)`, in a fresh dir.
    fn outcome(case: &str, play: &str, metas: &[(&str, &str)]) -> Vec<Outcome> {
        let d = std::env::temp_dir().join(format!("ansible-lsp-t153-{case}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for (name, text) in metas {
            std::fs::write(d.join(name), text).unwrap();
        }
        let text = format!("- hosts: localhost\n{play}  tasks: []\n");
        let path = d.join("site.yml");
        std::fs::write(&path, &text).unwrap();
        let nodes = Document::new(text.clone()).parse().unwrap();
        sites(&nodes, &text, &path, &ScanCache::default()).into_iter().map(|s| s.outcome).collect()
    }

    fn fault(o: &[Outcome]) -> String {
        match o {
            [Outcome::Fault(m)] => m.clone(),
            other => panic!("expected one fault, got {other:?}"),
        }
    }

    fn found(o: &[Outcome]) -> String {
        match o {
            [Outcome::Found { meta, .. }] => file_name(meta),
            other => panic!("expected one spec, got {other:?}"),
        }
    }

    /// Every row measured on 2.21.2 with `scratchpad/t153/probe.sh`.
    #[test]
    fn each_validate_argspec_shape_gets_ansibles_answer() {
        let prefix = "Error processing keyword 'validate_argspec': ";
        let meta_needed = "A playbook meta file is required. Considered: site.meta.yml, \
                           site.meta.yaml, site.meta.json, site.meta";
        let name_needed = "A play name is required when validate_argspec is True. Alternatively, \
                           set validate_argspec to the name of an argument spec.";

        let o = outcome("v01", "  name: named play\n  validate_argspec: true\n", &[]);
        assert_eq!(fault(&o), format!("{prefix}{meta_needed}"));
        let o = outcome("v02", "  validate_argspec: true\n", &[]);
        assert_eq!(fault(&o), format!("{prefix}{name_needed} {meta_needed}"));
        let o = outcome("v03", "  validate_argspec: true\n", &[("site.meta.yml", SPEC)]);
        assert_eq!(fault(&o), format!("{prefix}{name_needed}"));
        let o = outcome("v04", "  validate_argspec: nope\n", &[("site.meta.yml", SPEC)]);
        assert!(fault(&o).starts_with(&format!("{prefix}No argument spec named 'nope' in site.meta.yml.")), "{o:?}");
        let o = outcome("v05", "  validate_argspec: myspec\n", &[("site.meta.yml", "argument_specs:\n  myspec:\n    description: x\n")]);
        assert!(fault(&o).contains("No argument spec named 'myspec'"), "{o:?}");
        // A quoted `"true"` is a spec name, not the boolean.
        let o = outcome("v12", "  validate_argspec: \"true\"\n", &[("site.meta.yml", SPEC)]);
        assert!(fault(&o).contains("No argument spec named 'true'"), "{o:?}");
        // The first candidate that exists is the one read, even without the spec in it.
        let o = outcome(
            "v16",
            "  validate_argspec: myspec\n",
            &[("site.meta.yml", "argument_specs:\n  other:\n    options: {}\n"), ("site.meta.yaml", SPEC)],
        );
        assert!(fault(&o).contains("in site.meta.yml"), "{o:?}");

        // The ones that load.
        assert_eq!(found(&outcome("v07", "  validate_argspec: myspec\n", &[("site.meta.yml", SPEC)])), "site.meta.yml");
        assert_eq!(found(&outcome("v08", "  name: named play\n  validate_argspec: true\n", &[("site.meta.yml", SPEC)])), "site.meta.yml");
        assert_eq!(found(&outcome("v09", "  validate_argspec: myspec\n", &[("site.meta.yaml", SPEC)])), "site.meta.yaml");
        assert_eq!(found(&outcome("v10", "  validate_argspec: myspec\n", &[("site.meta", SPEC)])), "site.meta");
        let json = r#"{"argument_specs": {"myspec": {"options": {}}}}"#;
        assert_eq!(found(&outcome("v11", "  validate_argspec: myspec\n", &[("site.meta.json", json)])), "site.meta.json");
        assert_eq!(found(&outcome("v13", "  name: named play\n  validate_argspec: yes\n", &[("site.meta.yml", SPEC)])), "site.meta.yml");

        // And the ones that never look.
        assert!(outcome("v06", "  validate_argspec: false\n", &[]).is_empty());
        assert!(outcome("v14", "  vars: {s: myspec}\n  validate_argspec: \"{{ s }}\"\n", &[]).is_empty());
        assert!(outcome("v15", "  validate_argspec: \"\"\n", &[]).is_empty());
        assert!(outcome("none", "  name: no keyword\n", &[]).is_empty());
    }

    /// The jump target is the spec's key, so go-to lands on the entry rather than the file.
    #[test]
    fn a_found_spec_points_at_its_key() {
        let d = std::env::temp_dir().join("ansible-lsp-t153-entry");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("site.meta.yml"), SPEC).unwrap();
        let text = "- hosts: localhost\n  validate_argspec: myspec\n  tasks: []\n";
        let path = d.join("site.yml");
        std::fs::write(&path, text).unwrap();
        let nodes = Document::new(text.into()).parse().unwrap();
        let got = sites(&nodes, text, &path, &ScanCache::default());
        let [Site { value, outcome: Outcome::Found { entry, .. } }] = got.as_slice() else { panic!("{got:?}") };
        assert_eq!(value.slice(text), "myspec");
        assert_eq!(entry.slice(SPEC), "myspec");
    }
}
