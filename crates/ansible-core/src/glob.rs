//! Templated paths: `"{{ ap_protocol }}/validate.yml"` -> every file matching
//! `*/validate.yml`.
//!
//! The target genuinely depends on runtime variables, so this can only ever offer
//! candidates — never assert one is correct, and never warn when nothing matches.

use std::path::{Path, PathBuf};

use crate::fs::{Fs, StdFs};

/// Cap on returned matches. A pattern that explodes is unhelpful as a jump list.
const MAX_MATCHES: usize = 20;

/// `{{ … }}` -> `*`.
fn to_pattern(value: &str) -> String {
    let mut out = String::new();
    let mut rest = value;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        out.push('*');
        match rest[start..].find("}}") {
            Some(end) => rest = &rest[start + end + 2..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Does `name` match a single path component pattern containing `*`?
fn matches(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let mut pos = 0usize;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            if !name.starts_with(part) {
                return false;
            }
            pos = part.len();
        } else if i == parts.len() - 1 {
            return name.len() >= pos + part.len() && name[pos..].ends_with(part);
        } else {
            match name[pos..].find(part) {
                Some(at) => pos += at + part.len(),
                None => return false,
            }
        }
    }
    true
}

/// Expand one component at a time, reading the directory only where a wildcard sits.
fn expand(base: &Path, components: &[&str], fs: &dyn Fs, out: &mut Vec<PathBuf>) {
    if out.len() >= MAX_MATCHES {
        return;
    }
    let Some((head, tail)) = components.split_first() else {
        if fs.is_file(base) {
            out.push(base.to_path_buf());
        }
        return;
    };

    if !head.contains('*') {
        return expand(&base.join(head), tail, fs, out);
    }

    let mut names: Vec<String> = fs
        .read_dir(base)
        .into_iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .filter(|n| matches(head, n))
        .collect();
    names.sort();
    for name in names {
        expand(&base.join(name), tail, fs, out);
    }
}

/// Candidate files for a templated value, searched across `bases`.
///
/// Returns nothing when the pattern has no literal text left to anchor on —
/// `"{{ x }}.yml"` would otherwise match every YAML file in the tree.
pub fn candidates(bases: &[PathBuf], value: &str) -> Vec<PathBuf> {
    candidates_in(bases, value, &StdFs)
}

/// [`candidates`] against a caller-supplied filesystem (T-085).
pub fn candidates_in(bases: &[PathBuf], value: &str, fs: &dyn Fs) -> Vec<PathBuf> {
    let pattern = to_pattern(value);
    // The extension is not anchor text: "{{ x }}.yml" -> "*.yml" would otherwise
    // look anchored by "yml" and match every task file.
    let stem = pattern
        .strip_suffix(".yml")
        .or_else(|| pattern.strip_suffix(".yaml"))
        .unwrap_or(&pattern);
    let literal: String = stem.replace('*', "").replace(['/', '.', '_', '-'], "");
    if literal.len() < 2 {
        return Vec::new();
    }

    let components: Vec<&str> = pattern.split('/').filter(|c| !c.is_empty()).collect();
    let mut out = Vec::new();
    for base in bases {
        expand(base, &components, fs, &mut out);
        if out.len() >= MAX_MATCHES {
            break;
        }
    }
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jinja_becomes_a_wildcard() {
        assert_eq!(to_pattern("{{ ap_protocol }}/validate.yml"), "*/validate.yml");
        assert_eq!(
            to_pattern("protocol-base/validate-{{ op }}.yml"),
            "protocol-base/validate-*.yml"
        );
        assert_eq!(to_pattern("plain.yml"), "plain.yml");
    }

    #[test]
    fn component_matching() {
        assert!(matches("validate-*.yml", "validate-create.yml"));
        assert!(!matches("validate-*.yml", "converge.yml"));
        assert!(matches("*", "anything"));
        assert!(matches("_*.yml", "_converge.yml"));
        assert!(!matches("_*.yml", "converge.yml"));
    }

    /// `"{{ x }}.yml"` has nothing to anchor on and would match every file.
    #[test]
    fn unanchored_patterns_match_nothing() {
        let base = vec![PathBuf::from(env!("CARGO_MANIFEST_DIR"))];
        // The extension must not count as anchor text.
        assert!(candidates(&base, "{{ anything }}.yml").is_empty());
        assert!(candidates(&base, "{{ anything }}.yaml").is_empty());
        assert!(candidates(&base, "{{ a }}/{{ b }}").is_empty());
        assert!(candidates(&base, "{{ a }}_{{ b }}.yml").is_empty());
    }

    #[test]
    fn finds_real_matches_in_the_repo() {
        let Ok(home) = std::env::var("HOME") else { return };
        let tasks = PathBuf::from(home).join("app/ansible/roles/sync-state/tasks");
        if !tasks.is_dir() {
            return;
        }
        // Three protocol dirs each hold _converge_one_ap.yml.
        let hits = candidates(&[tasks], "{{ proto }}_access_point/_converge_one_ap.yml");
        assert!(hits.len() >= 2, "expected several protocol dirs, got {hits:?}");
        assert!(hits.iter().all(|p| p.ends_with("_converge_one_ap.yml")));
    }
}
