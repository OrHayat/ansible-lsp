//! T-062: the inventory as a variable source.
//!
//! Inventory is where a large share of real variables live — `infiniband_ip`, `hostname`,
//! `private_ip` in the corpus this was built against — and until now the index could not
//! see any of it. Every `var-undefined` message concedes inventory for exactly this reason.
//!
//! **Which file** is the hard half, not the parsing. Ansible resolves
//! `-i` → `ANSIBLE_INVENTORY` → `ansible.cfg [defaults] inventory` → `/etc/ansible/hosts`,
//! taking the **highest level that is set and only that** — measured: an env-named
//! inventory plus a `-i` runs the `-i` hosts alone, and `/etc/ansible/hosts` is never
//! consulted once anything else is. Within a level it is plural: several `-i`, a comma list
//! in the config, or a directory whose files are all read.
//!
//! We can observe every level but the first. `-i` is a runtime argument that never reaches
//! an editor, so [`sources`] takes an override for it — the `ansibleLsp.inventory` setting,
//! the user stating what they actually run with. Without one we model a plain
//! `ansible-playbook` invocation, which is the honest default.
//!
//! A configured inventory that does not exist is **normal**, not an error: the corpus this
//! was built against generates its inventories and commits none of them, so a clean clone
//! has none. Resolve, find nothing, say nothing.

use std::path::{Path, PathBuf};

use crate::config::AnsibleConfig;
use crate::fs::Fs;
use crate::parse::{Node, Span};

/// Ansible's own fallback when nothing names an inventory (`base.yml:799`).
const DEFAULT_HOST_LIST: &str = "/etc/ansible/hosts";

/// What a source turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `[group]` / `[group:vars]` sections.
    Ini,
    /// `all: / children: / hosts: / vars:` nesting.
    Yaml,
    /// A plugin config or an executable script. **Never read, never run** — see [`classify`].
    Dynamic,
}

/// A variable an inventory defines, with the span of its **value** so hover can show it.
#[derive(Debug, Clone)]
pub struct InventoryVar {
    pub name: String,
    pub span: Span,
}

/// The inventory sources in effect, in load order, with directories expanded to their
/// files.
///
/// Only one level of the ladder contributes — they do not merge, which is what ansible does
/// and why this returns one list rather than a union. `cfg.inventory` already carries the
/// whole resolution: `config.rs` lets the env beat the file, and the editor's
/// `ansibleLsp.inventory` setting (standing in for `-i`) is applied over both by the caller
/// that owns it, so this sees a single settled answer and only has to supply the default.
///
/// A path that does not exist is dropped silently — the normal state where inventories are
/// generated and untracked.
pub fn sources(cfg: &AnsibleConfig, fs: &dyn Fs) -> Vec<PathBuf> {
    let chosen: Vec<PathBuf> = match &cfg.inventory {
        Some(v) => v.clone(),
        None => vec![PathBuf::from(DEFAULT_HOST_LIST)],
    };
    let mut out = Vec::new();
    for p in chosen {
        if fs.is_dir(&p) {
            // A directory source reads every file in it — measured, two inventories in one
            // directory produced both hosts.
            let mut files: Vec<PathBuf> = fs
                .read_dir(&p)
                .into_iter()
                .filter(|(_, k)| *k == crate::fs::Kind::File)
                .map(|(f, _)| f)
                .collect();
            files.sort();
            out.extend(files);
        } else if fs.is_file(&p) {
            out.push(p);
        }
    }
    out
}

/// What this source is, from its content — never from its extension, which ansible does not
/// trust either.
///
/// [`Kind::Dynamic`] is the one that matters: a plugin config (a top-level `plugin:` key) or
/// an executable file is something ansible *runs*, and we will not. Running arbitrary code
/// out of a workspace to learn a host list is not a trade worth making, so we skip it and
/// accept that our view is incomplete.
///
/// A generator script is **not** this: it writes an inventory file that ansible then reads
/// as an ordinary static source. The test is what ansible's own is — what the inventory
/// *points at* — so a `.py` in the repo that nothing names is never classified here at all.
pub fn classify(path: &Path, nodes: &[Node], fs: &dyn Fs) -> Kind {
    if fs.is_executable(path) {
        return Kind::Dynamic;
    }
    if nodes.iter().any(|n| n.get("plugin").is_some()) {
        return Kind::Dynamic;
    }
    // A mapping is a YAML inventory; everything else is read as INI, which is also the
    // order Ansible falls back in — its ini plugin accepts any readable non-`.toml` file
    // (`plugins/inventory/ini.py:108-110`), so INI is the catch-all rather than a guess.
    if nodes.iter().any(|n| matches!(n, Node::Mapping { .. })) {
        Kind::Yaml
    } else {
        Kind::Ini
    }
}

/// Every variable a YAML inventory defines — group vars under `vars:`, host vars under each
/// entry of `hosts:`, recursing through `children:`.
///
/// Group and host names themselves are not variables and are not returned; only the leaves.
pub fn yaml_vars(nodes: &[Node]) -> Vec<InventoryVar> {
    fn group(node: &Node, out: &mut Vec<InventoryVar>) {
        for (k, v) in node.entries() {
            match k.as_str() {
                Some("vars") => bindings(v, out),
                Some("hosts") => {
                    // Each entry is a host; its mapping is that host's variables. A host
                    // with no vars is a null value, which has no entries and is skipped.
                    for (_, hv) in v.entries() {
                        bindings(hv, out);
                    }
                }
                Some("children") => {
                    for (_, gv) in v.entries() {
                        group(gv, out);
                    }
                }
                // A top-level group name (`all:`, or a bare group) — recurse into it.
                _ => group(v, out),
            }
        }
    }
    fn bindings(node: &Node, out: &mut Vec<InventoryVar>) {
        for (k, v) in node.entries() {
            if let Some(name) = k.as_str() {
                out.push(InventoryVar { name: name.to_string(), span: v.span() });
            }
        }
    }
    let mut out = Vec::new();
    for n in nodes {
        group(n, &mut out);
    }
    out
}

/// Every variable an INI inventory defines: a `[group:vars]` section's `name=value` pairs,
/// and the inline `var=value` on a host line. `[group:children]` names groups, not
/// variables, and contributes none.
pub fn ini_vars(text: &str) -> Vec<InventoryVar> {
    let mut out = Vec::new();
    // None = the implicit ungrouped-hosts section every ini inventory starts in.
    let mut in_vars_section = false;
    let mut at = 0usize;
    for line in text.split_inclusive('\n') {
        let start = at;
        at += line.len();
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if let Some(header) = trimmed.strip_prefix('[').and_then(|h| h.split(']').next()) {
            in_vars_section = header.ends_with(":vars");
            // `:children` lists group names; nothing in it is a variable, and the flag
            // above already excludes it from the pair reader below.
            continue;
        }
        // Offset of `trimmed` within the document, so value spans are absolute.
        let indent = line.len() - line.trim_start().len();
        let base = start + indent;
        if in_vars_section {
            if let Some(v) = pair(trimmed, base) {
                out.push(v);
            }
            continue;
        }
        // A host line: the first token is the host name, the rest are `k=v` pairs.
        let mut cursor = base;
        for (i, tok) in trimmed.split_whitespace().enumerate() {
            let tok_at = base + trimmed.find(tok).map_or(cursor - base, |o| o);
            cursor = tok_at + tok.len();
            if i == 0 {
                continue;
            }
            if let Some(v) = pair(tok, tok_at) {
                out.push(v);
            }
        }
    }
    out
}

/// One `name=value`, with the span covering the value. `None` when there is no `=`, which
/// on a host line is a connection token rather than a variable.
fn pair(s: &str, base: usize) -> Option<InventoryVar> {
    let eq = s.find('=')?;
    let name = s[..eq].trim();
    if name.is_empty() || name.contains(char::is_whitespace) {
        return None;
    }
    let value_start = eq + 1;
    Some(InventoryVar {
        name: name.to_string(),
        span: Span { start: base + value_start, end: base + s.len() },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::Document;

    fn names(vars: &[InventoryVar]) -> Vec<&str> {
        vars.iter().map(|v| v.name.as_str()).collect()
    }

    /// The measured shapes, from the probe that ran every source at once: a `[group:vars]`
    /// section and the inline `var=value` on a host line both reach the play.
    #[test]
    fn ini_reads_group_vars_sections_and_host_line_pairs() {
        let src = concat!(
            "# a comment\n",
            "[webservers]\n",
            "node1 ansible_connection=local host_line_var=FROM_HOST_LINE\n",
            "\n",
            "[webservers:vars]\n",
            "group_vars_section=FROM_GROUP_VARS_SECTION\n",
            "[webservers:children]\n",
            "leafs\n",
        );
        let got = ini_vars(src);
        assert_eq!(
            names(&got),
            ["ansible_connection", "host_line_var", "group_vars_section"]
        );
        // Spans point at the value, so hover can show it.
        let v = got.iter().find(|v| v.name == "host_line_var").unwrap();
        assert_eq!(v.span.slice(src), "FROM_HOST_LINE");
        // A `:children` member is a group name, not a variable.
        assert!(!names(&got).contains(&"leafs"));
    }

    /// The corpus shape: `all:` with `vars:`, and hosts nested under `children:`.
    #[test]
    fn yaml_reads_group_vars_and_nested_host_vars() {
        let src = concat!(
            "all:\n",
            "  vars:\n",
            "    lustre_version: 2.6\n",
            "  children:\n",
            "    lustre_servers:\n",
            "      hosts:\n",
            "        server1:\n",
            "          infiniband_ip: 192.168.100.4\n",
            "        server2:\n",
            "          infiniband_ip: 192.168.100.5\n",
        );
        let nodes = Document::new(src.to_string()).parse().expect("valid yaml");
        let got = yaml_vars(&nodes);
        assert_eq!(names(&got), ["lustre_version", "infiniband_ip", "infiniband_ip"]);
        assert_eq!(got[1].span.slice(src), "192.168.100.4");
        // Group and host names are structure, not variables.
        assert!(!names(&got).contains(&"lustre_servers"));
        assert!(!names(&got).contains(&"server1"));
    }

    /// A host with no variables is a null value, and must not panic or invent a name.
    #[test]
    fn a_bare_host_contributes_nothing() {
        let nodes = Document::new("all:\n  hosts:\n    node1:\n".to_string()).parse().unwrap();
        assert!(yaml_vars(&nodes).is_empty());
    }
}

