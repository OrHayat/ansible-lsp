//! Templated paths: `"{{ ap_protocol }}/validate.yml"` -> every file matching
//! `*/validate.yml`.
//!
//! The target genuinely depends on runtime variables, so this can only ever offer
//! candidates — never assert one is correct, and never warn when nothing matches.

use std::path::{Path, PathBuf};

use crate::fs::{Fs, Kind, StdFs};

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

    // With pattern left to match, only a directory can hold it — descending into a file
    // builds paths that cannot exist (`playbook.yml/tasks/sibling.yml`) and stats them.
    // The kind is free: the directory read already reported it.
    let mut names: Vec<String> = fs
        .read_dir(base)
        .into_iter()
        .filter(|(_, kind)| tail.is_empty() || *kind == Kind::Dir)
        .filter_map(|(p, _)| p.file_name().map(|n| n.to_string_lossy().to_string()))
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

/// The values a templated path's one variable takes across `targets`, the files
/// [`candidates_in`] found for it (T-071): `{{ protocol }}_target/check.yml` over
/// `http_target/check.yml` and `ftp_target/check.yml` gives `("protocol", ["ftp", "http"])`.
///
/// `None` unless the answer is the whole set: one bare variable, at most once per path
/// component, a glob that was not cut off at [`MAX_MATCHES`], and every target readable
/// back. A value containing `/` can reach files no single-component glob sees, so the set is
/// what is on disk for plain values, not a proof that nothing else works.
pub fn implied_values(value: &str, targets: &[PathBuf]) -> Option<(String, Vec<String>)> {
    if targets.is_empty() || targets.len() >= MAX_MATCHES {
        return None;
    }
    let mut var: Option<String> = None;
    let mut parts: Vec<(String, Option<String>)> = Vec::new();
    for comp in value.split('/').filter(|c| !c.is_empty()) {
        let Some(open) = comp.find("{{") else {
            parts.push((comp.to_string(), None));
            continue;
        };
        let close = comp[open..].find("}}")? + open;
        let name = comp[open + 2..close].trim();
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
        if var.as_deref().is_some_and(|v| v != name) || comp[close + 2..].contains("{{") {
            return None;
        }
        var = Some(name.to_string());
        parts.push((comp[..open].to_string(), Some(comp[close + 2..].to_string())));
    }
    let var = var?;
    let mut values: Vec<String> = Vec::new();
    for t in targets {
        let comps: Vec<String> = t.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
        let tail = comps.get(comps.len().checked_sub(parts.len())?..)?;
        let mut seen: Option<String> = None;
        for ((prefix, suffix), name) in parts.iter().zip(tail) {
            match suffix {
                None if prefix == name => {}
                None => return None,
                Some(suffix) => {
                    let v = name.strip_prefix(prefix.as_str())?.strip_suffix(suffix.as_str())?;
                    if seen.as_deref().is_some_and(|s| s != v) {
                        return None;
                    }
                    seen = Some(v.to_string());
                }
            }
        }
        values.push(seen?);
    }
    values.sort();
    values.dedup();
    Some((var, values))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// T-071. The values read back off the matched files, and the shapes that give no answer
    /// rather than a partial one.
    #[test]
    fn implied_values_reads_the_variable_back_off_each_match() {
        let p = |s: &str| PathBuf::from(s);
        let both = [p("/r/tasks/http_target/check.yml"), p("/r/tasks/ftp_target/check.yml")];
        assert_eq!(
            implied_values("{{ protocol }}_target/check.yml", &both),
            Some(("protocol".to_string(), vec!["ftp".to_string(), "http".to_string()]))
        );
        assert_eq!(
            implied_values("nested/{{ which }}.yml", &[p("/r/tasks/nested/deep.yml")]),
            Some(("which".to_string(), vec!["deep".to_string()]))
        );
        // The same variable twice must agree within one path.
        assert_eq!(
            implied_values("{{ v }}/{{ v }}.yml", &[p("/r/a/a.yml"), p("/r/b/b.yml")]),
            Some(("v".to_string(), vec!["a".to_string(), "b".to_string()]))
        );
        assert_eq!(implied_values("{{ v }}/{{ v }}.yml", &[p("/r/a/b.yml")]), None);
        assert_eq!(implied_values("{{ a }}/{{ b }}.yml", &[p("/r/x/y.yml")]), None, "two variables");
        assert_eq!(implied_values("{{ a.b }}_t/c.yml", &[p("/r/x_t/c.yml")]), None, "not a bare name");
        assert_eq!(implied_values("{{ a }}_t/c.yml", &[]), None, "nothing matched");
        let capped: Vec<PathBuf> = (0..MAX_MATCHES).map(|i| p(&format!("/r/{i}_t/c.yml"))).collect();
        assert_eq!(implied_values("{{ a }}_t/c.yml", &capped), None, "a cut-off glob is not the whole set");
    }

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

    /// A templated directory segment expands to every sibling that matches, which is the
    /// case navigation offers candidates for instead of warning. Built inline rather than
    /// read from a private repo under `$HOME` (T-077).
    #[test]
    fn a_templated_directory_segment_matches_every_sibling() {
        let root = crate::testing::tree(
            "glob-protocols",
            &[
                ("tasks/http_access_point/_converge_one_ap.yml", ""),
                ("tasks/ftp_access_point/_converge_one_ap.yml", ""),
                ("tasks/nfs_access_point/_converge_one_ap.yml", ""),
                // Same directory shape, different leaf: must not be swept up by the pattern.
                ("tasks/http_access_point/other.yml", ""),
                // Matches the leaf but not the directory pattern.
                ("tasks/unrelated/_converge_one_ap.yml", ""),
            ],
        );
        let hits = candidates(
            &[root.join("tasks")],
            "{{ proto }}_access_point/_converge_one_ap.yml",
        );
        assert_eq!(hits.len(), 3, "one per protocol dir, got {hits:?}");
        assert!(hits.iter().all(|p| p.ends_with("_converge_one_ap.yml")));
        assert!(
            !hits.iter().any(|p| p.to_string_lossy().contains("unrelated")),
            "the directory segment must constrain the match too: {hits:?}"
        );
    }
}
