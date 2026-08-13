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
//! in the config, or a directory — which is walked recursively, minus dotfiles, a fixed
//! list of extensions, and `group_vars`/`host_vars`. See [`expand_dir`].
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

/// Suffixes a **directory** source refuses — `INVENTORY_IGNORE_EXTS`, measured on 2.21.2
/// with one file per suffix in one directory and the host list read back.
///
/// `.ini` is deliberately absent, which is the whole reason this list is measured rather
/// than remembered: `.ini` sits in `MODULE_IGNORE_EXTS`, a different list with a confusingly
/// similar name, and a `.ini` inside an inventory directory is read like anything else.
/// `.cfg` is here, so an `ansible.cfg` that wanders into an inventory directory is not a
/// host list — dropping it is what keeps us from inventing variables out of it.
const IGNORE_EXTS: &[&str] = &[
    ".pyc", ".pyo", ".swp", ".bak", "~", ".rpm", ".md", ".txt", ".rst", ".orig", ".cfg", ".retry",
];

/// Directories a directory source never descends. `group_vars`/`host_vars` still contribute
/// variables — they are read as variable directories by [`crate::vars`] — they are just
/// never parsed as host lists, so a `group_vars/all.yml` is not also an inventory file.
const IGNORE_DIRS: &[&str] = &["group_vars", "host_vars", "vars_plugins"];

/// A symlinked directory pointing at an ancestor would otherwise recurse until the stack
/// goes. Deeper than this is not a real inventory layout.
const MAX_DEPTH: usize = 32;

/// Does expanding a directory source step over this entry? One predicate rather than the
/// rule restated at each site: the reader, the discovery sniff and the picker's "skips…"
/// line all ask this, and two of them disagreeing is how a picker comes to advertise a file
/// the reader never loads.
pub fn ignored_entry(name: &str) -> bool {
    name.starts_with('.') || IGNORE_EXTS.iter().any(|e| name.ends_with(e))
}

/// Does a directory source step over this whole directory? Same reason as
/// [`ignored_entry`]: discovery must not offer a `group_vars/` as a folder inventory, since
/// selecting it would read nothing at all.
pub fn ignored_dir(name: &str) -> bool {
    name.starts_with('.') || IGNORE_DIRS.contains(&name)
}

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
            out.extend(expand(&p, fs));
        } else if fs.is_file(&p) {
            out.push(p);
        }
    }
    out
}

/// Every file a directory source contributes, in load order.
///
/// Measured on 2.21.2 against one directory holding a file per rule, reading back which
/// hosts arrived: subdirectories **are** descended, dotfiles and [`IGNORE_EXTS`] are
/// skipped, and [`IGNORE_DIRS`] are stepped over. None of it applies to a path the user
/// named directly — `-i x.cfg` is read, `x.cfg` inside a directory is not — which is why
/// the filtering lives here and not in [`sources`].
///
/// Public because the picker shows a folder's contents before you commit to it. That view
/// and the reader must never disagree, so they are the same function.
pub fn expand(dir: &Path, fs: &dyn Fs) -> Vec<PathBuf> {
    let mut out = Vec::new();
    expand_dir(dir, fs, 0, &mut out);
    out
}

fn expand_dir(dir: &Path, fs: &dyn Fs, depth: usize, out: &mut Vec<PathBuf>) {
    if depth >= MAX_DEPTH {
        return;
    }
    let mut entries = fs.read_dir(dir);
    // By entry name, and subdirectories expanded where their own name sorts rather than
    // after every file: ansible walks `sorted(os.listdir())` and recurses as it goes, and
    // this order is what breaks a same-level collision between two of these files.
    entries.sort_by(|a, b| a.0.file_name().cmp(&b.0.file_name()));
    for (path, kind) in entries {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if kind == crate::fs::Kind::Dir {
            if !ignored_dir(name) {
                expand_dir(&path, fs, depth + 1, out);
            }
        } else if !ignored_entry(name) {
            out.push(path);
        }
    }
}

/// Is this file worth *offering* as an inventory? A discovery question, and deliberately
/// not [`classify`]'s.
///
/// [`classify`] answers "how do I parse a file already named as a source" and falls back to
/// [`Kind::Ini`] for anything it cannot make sense of — correct there, useless here, since
/// it would make every text file in the workspace a candidate. This one has to be able to
/// say **no**, so it asks for a positive signal:
///
/// - a YAML mapping with an `all:` or `plugin:` key, or any entry whose value has
///   `hosts:`/`children:` — the group shape. A playbook is a *sequence*, so it cannot
///   match; a `group_vars/x.yml` is a mapping of names to values, so it does not either.
/// - an INI `[section]` header.
///
/// Extension is still consulted, but only to reject: a file ansible would refuse inside a
/// directory ([`IGNORE_EXTS`]) is never offered, which is what keeps `ansible.cfg` — a real
/// INI file full of `[section]` headers — out of the list.
pub fn looks_like_inventory(path: &Path, text: &str, nodes: &[Node]) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if ignored_entry(name) {
        return false;
    }
    let group_shaped = nodes.iter().any(|n| {
        n.entries().iter().any(|(k, v)| {
            matches!(k.as_str(), Some("all") | Some("plugin"))
                || v.get("hosts").is_some()
                || v.get("children").is_some()
        })
    });
    group_shaped
        || text
            .lines()
            .map(str::trim)
            .any(|l| l.starts_with('[') && l.ends_with(']') && l.len() > 2)
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

    /// Resolve `inventory = <value>` the way a user does — through `ansible.cfg` and the
    /// config reader — rather than by setting the field. Asserting on a hand-built config
    /// would test [`sources`] against a shape nothing produces.
    fn sources_for(value: &str, files: &[(&str, &str)], dirs: &[&str]) -> Vec<String> {
        let cfg_text = format!("[defaults]\ninventory = {value}\n");
        let mut all: Vec<(&str, &str)> = vec![("/p/ansible.cfg", &cfg_text)];
        all.extend_from_slice(files);
        let fs = crate::testing::MemFs::with_dirs(&all, dirs);
        let cfg = AnsibleConfig::builder(Path::new("/p"))
            .fs(&fs)
            .env(&crate::config::EnvMap::empty())
            .load();
        sources(&cfg, &fs)
            .iter()
            .map(|p| p.strip_prefix("/p").unwrap_or(p).to_string_lossy().into_owned())
            .collect()
    }

    /// Measured on 2.21.2: one directory holding a file per suffix, reading back which hosts
    /// arrived. `.ini` is read — it lives in `MODULE_IGNORE_EXTS`, not this list — while
    /// `.cfg`, `.bak`, `.md` and a `~` backup are dropped.
    #[test]
    fn a_directory_source_skips_the_suffixes_ansible_ignores() {
        let got = sources_for(
            "inv",
            &[
                ("/p/inv/a.ini", ""),
                ("/p/inv/b.yml", ""),
                ("/p/inv/c", ""),
                ("/p/inv/e.cfg", ""),
                ("/p/inv/f.bak", ""),
                ("/p/inv/notes.md", ""),
                ("/p/inv/backup~", ""),
            ],
            &[],
        );
        assert_eq!(got, ["inv/a.ini", "inv/b.yml", "inv/c"]);
    }

    /// A file the user named directly keeps its ignored suffix — measured, `-i inv/e.cfg`
    /// reads. The filtering is a property of expanding a directory, not of the file.
    #[test]
    fn a_directly_named_file_keeps_an_ignored_suffix() {
        let got = sources_for("inv/e.cfg", &[("/p/inv/e.cfg", "")], &[]);
        assert_eq!(got, ["inv/e.cfg"]);
    }

    /// Measured: a `sub/g.yml` two levels down reached the play. Ordering is by entry name
    /// with the subdirectory expanded where its own name sorts, not appended after the
    /// files — the order decides a same-level collision between two of them.
    #[test]
    fn a_directory_source_descends_subdirectories_in_name_order() {
        let got = sources_for(
            "inv",
            &[("/p/inv/a.yml", ""), ("/p/inv/m/inner.yml", ""), ("/p/inv/z.yml", "")],
            &[],
        );
        assert_eq!(got, ["inv/a.yml", "inv/m/inner.yml", "inv/z.yml"]);
    }

    /// Measured: a perfectly parseable `group_vars/sneaky.ini` contributed no host. These
    /// directories still supply variables — as variable directories, read elsewhere — but
    /// are never host lists.
    #[test]
    fn a_directory_source_steps_over_group_vars_and_host_vars() {
        let got = sources_for(
            "inv",
            &[
                ("/p/inv/group_vars/all.yml", ""),
                ("/p/inv/host_vars/web01.yml", ""),
                ("/p/inv/vars_plugins/x.ini", ""),
                ("/p/inv/hosts.ini", ""),
            ],
            &[],
        );
        assert_eq!(got, ["inv/hosts.ini"]);
    }

    /// The discovery sniff, with its negative controls. Without those this test proves
    /// nothing: a predicate that says yes to everything passes every positive case.
    #[test]
    fn discovery_offers_inventories_and_refuses_everything_else() {
        fn sniff(name: &str, text: &str) -> bool {
            let nodes = Document::new(text.to_string()).parse().unwrap_or_default();
            looks_like_inventory(Path::new(name), text, &nodes)
        }

        assert!(sniff("db.ini", "[webservers]\nweb01\n"), "an INI section");
        assert!(sniff("x.yml", "all:\n  hosts:\n    web01:\n"), "the `all:` shape");
        assert!(sniff("x.yml", "webservers:\n  hosts:\n    web01:\n"), "a bare group");
        assert!(sniff("x.yml", "plugin: amazon.aws.aws_ec2\n"), "a plugin config");

        // A playbook is a sequence, so the group shape cannot match it.
        assert!(
            !sniff("site.yml", "- name: play\n  hosts: webservers\n  tasks: []\n"),
            "a playbook has `hosts:` and must still be refused"
        );
        assert!(!sniff("all.yml", "ntp_server: 10.0.0.1\napp_tier: prod\n"), "a group_vars file");
        // Real INI, full of sections, and refused on its extension alone.
        assert!(!sniff("ansible.cfg", "[defaults]\ninventory = inv\n"), "ansible.cfg");
        assert!(!sniff("README.md", "[a link](x)\n"), "a markdown file");
        assert!(!sniff(".hidden.ini", "[webservers]\nweb01\n"), "a dotfile");
    }

    #[test]
    fn a_directory_source_skips_hidden_files() {
        let got = sources_for("inv", &[("/p/inv/.hidden.ini", ""), ("/p/inv/hosts.ini", "")], &[]);
        assert_eq!(got, ["inv/hosts.ini"]);
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
