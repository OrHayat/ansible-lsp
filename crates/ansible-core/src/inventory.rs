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
    /// `[group.vars]` / `[group.hosts.name]` tables — see [`toml_vars`].
    ///
    /// Its own kind rather than falling through to [`Kind::Ini`], which is where it landed
    /// before. That produced nothing, since TOML's spaced `key = value` reads as a bare
    /// host name to the INI reader — silence by accident, and an accident is not a rule.
    Toml,
}

/// A variable an inventory defines, with the span of its **value** so hover can show it.
#[derive(Debug, Clone)]
pub struct InventoryVar {
    pub name: String,
    pub span: Span,
}

/// The key ansible reads as a merge-order control instead of storing as a variable — in a
/// **group** position only. `Group.set_variable` intercepts it (`inventory/group.py:216-217`);
/// `Host.set_variable` (`inventory/host.py:119-130`) has no such branch, so on a host it is an
/// ordinary variable and stays indexed. Measured across all three formats and every position
/// these readers walk — see `ini_vars_drops_group_priority_from_every_vars_section` for the
/// table (T-178).
pub(crate) const GROUP_PRIORITY: &str = "ansible_group_priority";

/// Whether a collector is walking a group's `vars` or a host's own variables. Named rather
/// than a `bool`, for the reason [`ini_vars`]'s `Section` is: `bindings(v, true, out)` at a
/// call site says nothing, and this flag is exactly the thing that must not be got wrong.
///
/// It lives on the *caller*, never inside the collector — both collectors serve both
/// positions, so a filter one level down would drop the key from hosts too, which is the
/// mistake T-178 originally prescribed.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum GroupPosition {
    Yes,
    No,
}

impl GroupPosition {
    /// Does ansible eat this key here instead of defining it?
    fn consumes(self, name: &str) -> bool {
        self == Self::Yes && name == GROUP_PRIORITY
    }
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

/// The directory each configured source hands to the vars plugins as its base.
///
/// Ansible's `basedir()` returns a directory source *itself*, and a file source's parent —
/// so the `group_vars/` for `-i inv` is `inv/group_vars`, and it stays there however deep
/// inside `inv` the actual host files sit. Measured: with `inv/sub/hosts.ini` as the only
/// host file, `inv/group_vars/web.yml` still applies and `inv/sub/group_vars/web.yml` does
/// not. Deriving this per *expanded file* got both halves wrong at once.
pub fn source_dirs(cfg: &AnsibleConfig, fs: &dyn Fs) -> Vec<PathBuf> {
    let chosen: Vec<PathBuf> = match &cfg.inventory {
        Some(v) => v.clone(),
        None => vec![PathBuf::from(DEFAULT_HOST_LIST)],
    };
    let mut out = Vec::new();
    for p in chosen {
        let dir = if fs.is_dir(&p) {
            Some(p)
        } else if fs.is_file(&p) {
            p.parent().map(|d| d.to_path_buf())
        } else {
            None
        };
        if let Some(d) = dir {
            if !out.contains(&d) {
                out.push(d);
            }
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
/// - a TOML table holding `vars`/`hosts`/`children` — TOML is decided by its own grammar,
///   because every `[table]` satisfies the INI test and would let any TOML file through.
///
/// Extension is still consulted, but only to reject: a file ansible would refuse inside a
/// directory ([`IGNORE_EXTS`]) is never offered, which is what keeps `ansible.cfg` — a real
/// INI file full of `[section]` headers — out of the list.
pub fn looks_like_inventory(path: &Path, text: &str, nodes: &[Node]) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if ignored_entry(name) {
        return false;
    }
    // TOML answers from its own grammar, not from the line sniff below. Every `[table]` in a
    // TOML file looks like an INI section header, so the generic test accepts any TOML file
    // at all — it offered this repo's four `Cargo.toml`s as candidate inventories, `[package]`
    // and `[dependencies]` reading as groups. We already parse TOML properly for the same
    // files, so the honest test is the structure `toml_vars` walks: a table holding `vars`,
    // `hosts` or `children`.
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("toml")) {
        let Ok(doc) = toml_edit::Document::parse(text) else { return false };
        return doc.as_table().iter().any(|(_, item)| {
            item.as_table_like().is_some_and(|g| {
                ["vars", "hosts", "children"].iter().any(|k| g.get(k).is_some())
            })
        });
    }
    let group_shaped = nodes.iter().any(|n| {
        n.entries().iter().any(|(k, v)| {
            matches!(k.as_str(), Some("all") | Some("plugin"))
                || v.get("hosts").is_some()
                || v.get("children").is_some()
        })
    });
    // `trim_end`, deliberately not `trim`: an INI section header sits at column 0, so a
    // leading space is enough to disqualify a line. That is the difference between a host
    // list and `demo/tasks/lenient_scalar.yml`, whose `[_beacon_mode]` is a Jinja list
    // indented inside a folded scalar — offered as an inventory until this stopped
    // trimming the left.
    group_shaped
        || text.lines().any(|l| {
            let l = l.trim_end();
            l.starts_with('[') && l.ends_with(']') && l.len() > 2
        })
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
pub fn classify(path: &Path, text: &str, nodes: &[Node], fs: &dyn Fs) -> Kind {
    // The mode bit alone is not the test. `script.verify_file` accepts any executable, but the
    // manager only `break`s on a plugin that *succeeds* — a failing one is caught and the next
    // plugin tries the same file (`inventory/manager.py`). Measured: an INI inventory at mode
    // 755 is executed, fails, and is then read as INI, resolving exactly as it does at 644.
    // So "executable" alone meant we dropped whole inventories on any checkout where the mode
    // bits are noise — a bind mount, exFAT, someone's `chmod -R 755` — and every variable in
    // them read as undefined.
    //
    // A shebang is the discriminator, and it has to win over the content sniff rather than the
    // other way round: a shell script's `[ -f /etc/x ]` line satisfies the INI test on its own,
    // and an `export FOO=bar` beside it would have invented `FOO`.
    if fs.is_executable(path) && (text.starts_with("#!") || !looks_like_inventory(path, text, nodes))
    {
        return Kind::Dynamic;
    }
    // By extension, which is what ansible's own plugins do here: `ini.verify_file` accepts
    // any readable file EXCEPT `.toml` (`plugins/inventory/ini.py:108-110`), leaving it to
    // the toml plugin behind it.
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("toml")) {
        return Kind::Toml;
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

/// Every variable a TOML inventory defines.
///
/// The structure is the YAML one — top-level keys are groups, each holding `vars`, `hosts`
/// and `children` (`plugins/inventory/toml.py::_parse_group`) — with one difference that
/// matters here: `children` is a **list of group names**, not a nested mapping, so nothing
/// in it is ever a variable. Any other key is skipped, which is what the plugin does too.
///
/// Parsed by `toml_edit` rather than by hand. Ansible passes the whole file to `tomllib`
/// (`toml.py:155`), so it accepts the entire TOML grammar — multi-line strings, arrays,
/// inline tables, dotted keys. A line reader would cover a subset and would have to detect
/// everything outside it to stay quiet, which is most of the way to parsing anyway.
pub fn toml_vars(text: &str) -> Vec<InventoryVar> {
    let Ok(doc) = toml_edit::Document::parse(text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (_group, item) in doc.as_table().iter() {
        let Some(group) = item.as_table_like() else { continue };
        if let Some(vars) = group.get("vars").and_then(|i| i.as_table_like()) {
            collect_toml(vars, text, GroupPosition::Yes, &mut out);
        }
        if let Some(hosts) = group.get("hosts").and_then(|i| i.as_table_like()) {
            for (_host, entry) in hosts.iter() {
                if let Some(host_vars) = entry.as_table_like() {
                    collect_toml(host_vars, text, GroupPosition::No, &mut out);
                }
            }
        }
    }
    out
}

/// One table's `name = value` pairs, with the span covering the value.
///
/// The quotes are trimmed off a string so hover shows `10.0.0.1` rather than `"10.0.0.1"`,
/// matching what the INI and YAML readers hand back for the same inventory.
fn collect_toml(
    table: &dyn toml_edit::TableLike,
    text: &str,
    position: GroupPosition,
    out: &mut Vec<InventoryVar>,
) {
    for (name, item) in table.iter() {
        if position.consumes(name) {
            continue;
        }
        let Some(range) = item.span() else { continue };
        let (mut start, mut end) = (range.start, range.end);
        let raw = text.get(start..end).unwrap_or("");
        if raw.len() >= 2
            && (raw.starts_with('"') && raw.ends_with('"')
                || raw.starts_with('\'') && raw.ends_with('\''))
        {
            start += 1;
            end -= 1;
        }
        out.push(InventoryVar { name: name.to_string(), span: Span { start, end } });
    }
}

/// Expand an inventory host pattern into the names ansible actually creates.
///
/// `web[01:05]` is five hosts, not one, and a rule that reports "no such host" without
/// expanding would flag `web03` — a false ERROR on a correct file, which is the one outcome
/// worth more than the feature. Measured on 2.21.2 via `ansible-inventory --list`:
///
/// | written              | hosts                                    |
/// | -------------------- | ---------------------------------------- |
/// | `web[01:05]`         | `web01`…`web05` — inclusive, padding kept |
/// | `db[1:3]`            | `db1`, `db2`, `db3` — no padding asked, none given |
/// | `rack[a:c]`          | `racka`, `rackb`, `rackc`                |
/// | `s[00:10:5]`         | `s00`, `s05`, `s10` — step, still inclusive |
/// | `web[01:02].example.com` | suffix survives                      |
/// | `r[1:2]-n[a:b]`      | all four — several ranges are a cartesian product |
///
/// The last row is why this recurses rather than expanding one bracket. Both readers need
/// it: measured, a YAML inventory expands `node[01:03]:` exactly the same way, which is not
/// something the ini plugin's ownership of the syntax would have suggested.
///
/// A bracket that does not parse as a range is left alone and the pattern returns as one
/// literal name — ansible fails such a file outright, so there is no host list to be wrong
/// about.
pub fn expand_host_pattern(pattern: &str) -> Option<Vec<String>> {
    let Some(open) = pattern.find('[') else {
        return Some(vec![pattern.to_string()]);
    };
    let Some(close) = pattern[open..].find(']').map(|i| open + i) else {
        return Some(vec![pattern.to_string()]);
    };
    let (prefix, rest) = (&pattern[..open], &pattern[close + 1..]);
    let Some(values) = range_values(&pattern[open + 1..close]) else {
        return Some(vec![pattern.to_string()]);
    };
    // Recurse on the tail so a second bracket multiplies out rather than surviving as text.
    let tails = expand_host_pattern(rest)?;
    if values.len().checked_mul(tails.len())? > MAX_PATTERN_HOSTS {
        return None;
    }
    Some(
        values
            .into_iter()
            .flat_map(|v| tails.iter().map(move |tail| format!("{prefix}{v}{tail}")).collect::<Vec<_>>())
            .collect(),
    )
}

/// Above this many hosts from one pattern, the list is reported unknowable instead of built.
///
/// Not a correctness limit — ansible expands whatever you write — but this runs inside the
/// diagnostics pass, and `web[1:1000000]` measured at a million strings in 360ms, with nested
/// brackets multiplying (`a[1:100]b[1:100]c[1:100]` is the same million). Refusing beyond the
/// cap costs a missed report on a fleet larger than any real one; truncating instead would
/// drop real hosts and report each as a typo, which is the failure this rule exists to avoid.
const MAX_PATTERN_HOSTS: usize = 65_536;

/// The values one `start:end` or `start:end:step` produces, or `None` if it is not a range.
fn range_values(inner: &str) -> Option<Vec<String>> {
    let mut parts = inner.split(':');
    let (start, end) = (parts.next()?, parts.next()?);
    let step: usize = match parts.next() {
        Some(s) => s.parse().ok().filter(|n| *n > 0)?,
        None => 1,
    };
    if parts.next().is_some() || start.is_empty() || end.is_empty() {
        return None;
    }
    // Alphabetic: single characters, ordered the way ansible orders them — `ascii_letters`,
    // so the whole lowercase run precedes the whole uppercase one. Not ASCII order.
    //
    // Measured, and the difference is not cosmetic: `web[a:C]` is **29 hosts** —
    // `weba`..`webz` then `webA`,`webB`,`webC` — because the endpoints are indices into that
    // string. Comparing the bytes instead gives `a`(97) > `C`(67), an empty range, and every
    // one of those 29 real hosts then reads as "no such host".
    const LETTERS: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    if let (Ok(a), Ok(b)) = (start.parse::<char>(), end.parse::<char>()) {
        if a.is_ascii_alphabetic() && b.is_ascii_alphabetic() {
            let (i, j) = (LETTERS.find(a)?, LETTERS.find(b)?);
            // A reversed range is empty, matching ansible: `web[5:1]` contributes no host.
            return Some(
                LETTERS[i..=j.max(i)]
                    .chars()
                    .take(if j >= i { j - i + 1 } else { 0 })
                    .step_by(step)
                    .map(|c| c.to_string())
                    .collect(),
            );
        }
    }
    let (a, b) = (start.parse::<usize>().ok()?, end.parse::<usize>().ok()?);
    // `01` asks for two digits; `1` asks for none. The width comes from how the *start* was
    // written, which is what keeps `web[01:05]` from expanding to `web1`.
    let width = if start.starts_with('0') { start.len() } else { 1 };
    Some((a..=b).step_by(step).map(|n| format!("{n:0width$}")).collect())
}

/// Every host name a YAML inventory declares, patterns expanded.
///
/// Only `hosts:` keys count. A group name is not a host — measured, `hostvars['web']` for a
/// *group* called `web` fails exactly like an unknown host — so `children:` is recursed for
/// the hosts inside it and contributes none of its own names.
pub fn yaml_hosts(nodes: &[Node]) -> Option<Vec<String>> {
    fn group(node: &Node, out: &mut Vec<String>) -> Option<()> {
        for (k, v) in node.entries() {
            match k.as_str() {
                Some("hosts") => {
                    for (h, _) in v.entries() {
                        if let Some(name) = h.as_str() {
                            out.extend(expand_host_pattern(name)?);
                        }
                    }
                }
                Some("children") => {
                    for (_, gv) in v.entries() {
                        group(gv, out)?;
                    }
                }
                _ => {}
            }
        }
        Some(())
    }
    let mut out = Vec::new();
    for n in nodes {
        for (_, g) in n.entries() {
            group(g, &mut out)?;
        }
    }
    Some(out)
}

/// Every host name an INI inventory declares: the first token of each host line, in the
/// implicit ungrouped section and in every `[group]` section, patterns expanded.
///
/// `[group:vars]` holds variables and `[group:children]` holds group names, so neither
/// contributes a host — the same three-state section walk [`ini_vars`] uses, for the same
/// reason it needed three states rather than a bool.
pub fn ini_hosts(text: &str) -> Option<Vec<String>> {
    // An unknown section tag discards the whole file for ansible, so it must define no hosts
    // either — the host list and the variable list have to agree about which files are real.
    if text.lines().any(|l| unknown_section_type(l.trim())) {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    let mut hosts_section = true;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if let Some(header) = trimmed.strip_prefix('[').and_then(|h| h.split(']').next()) {
            hosts_section = !header.ends_with(":vars") && !header.ends_with(":children");
            continue;
        }
        if !hosts_section {
            continue;
        }
        // The first token is the host; the rest are `k=v` pairs that `ini_vars` owns.
        if let Some((s, e)) = host_tokens(trimmed, 0).into_iter().next() {
            out.extend(expand_host_pattern(&trimmed[s..e])?);
        }
    }
    Some(out)
}

/// Every host name a TOML inventory declares — the keys of each `[group.hosts]` table.
pub fn toml_hosts(text: &str) -> Option<Vec<String>> {
    let Ok(doc) = toml_edit::Document::parse(text) else {
        return Some(Vec::new());
    };
    let mut out = Vec::new();
    for (_group, item) in doc.as_table().iter() {
        let Some(group) = item.as_table_like() else { continue };
        if let Some(hosts) = group.get("hosts").and_then(|i| i.as_table_like()) {
            for (host, _) in hosts.iter() {
                out.extend(expand_host_pattern(host)?);
            }
        }
    }
    Some(out)
}

/// Every variable a YAML inventory defines — group vars under `vars:`, host vars under each
/// entry of `hosts:`, recursing through `children:`.
///
/// Group and host names themselves are not variables and are not returned; only the leaves.
pub fn yaml_vars(nodes: &[Node]) -> Vec<InventoryVar> {
    fn group(node: &Node, out: &mut Vec<InventoryVar>) {
        for (k, v) in node.entries() {
            match k.as_str() {
                Some("vars") => bindings(v, GroupPosition::Yes, out),
                Some("hosts") => {
                    // Each entry is a host; its mapping is that host's variables. A host
                    // with no vars is a null value, which has no entries and is skipped.
                    for (_, hv) in v.entries() {
                        bindings(hv, GroupPosition::No, out);
                    }
                }
                Some("children") => {
                    for (_, gv) in v.entries() {
                        group(gv, out);
                    }
                }
                // Anything else is skipped, not walked. Ansible warns "Skipping unexpected
                // key (x) in group (g), only vars, children and hosts are valid"
                // (`yaml.py::_parse_group`) — so a typo like `var:` for `vars:` must define
                // nothing. Walking it invented names, and an invented name is one
                // `var-undefined` then stops reporting.
                _ => {}
            }
        }
    }
    fn bindings(node: &Node, position: GroupPosition, out: &mut Vec<InventoryVar>) {
        for (k, v) in node.entries() {
            if let Some(name) = k.as_str() {
                if position.consumes(name) {
                    continue;
                }
                out.push(InventoryVar { name: name.to_string(), span: v.span() });
            }
        }
    }
    let mut out = Vec::new();
    for n in nodes {
        // Every top-level key is a group name, whatever it is spelled — a file whose first
        // key is `vars:` declares a *group* called `vars`, measured, and its contents are
        // not variables. So the section names are only read one level in.
        for (_, g) in n.entries() {
            group(g, &mut out);
        }
    }
    out
}

/// Every variable an INI inventory defines: a `[group:vars]` section's `name=value` pairs,
/// and the inline `var=value` on a host line. `[group:children]` names groups, not
/// variables, and contributes none.
pub fn ini_vars(text: &str) -> Vec<InventoryVar> {
    /// What the lines under the current header are. Three states, not a `is_vars` bool:
    /// `:children` is neither a vars section nor a host list, and sharing `false` with
    /// `Hosts` meant its lines went to the host-line reader — harmless for a bare group
    /// name, but `leafs foo=bar` there invented `foo`.
    #[derive(PartialEq)]
    enum Section {
        Vars,
        Hosts,
        Skip,
    }
    // A section tag outside `hosts`/`children`/`vars` kills the WHOLE file, not the
    // section: ansible's reader raises on it (`ini.py:189-191`) and a failed plugin
    // defines nothing — measured, `an_unknown_section_type_discards_the_whole_ini_file`.
    if text.lines().any(|l| unknown_section_type(l.trim())) {
        return Vec::new();
    }
    let mut out = Vec::new();
    // Hosts = the implicit ungrouped-hosts section every ini inventory starts in.
    let mut section = Section::Hosts;
    let mut at = 0usize;
    for line in text.split_inclusive('\n') {
        let start = at;
        at += line.len();
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if let Some(header) = trimmed.strip_prefix('[').and_then(|h| h.split(']').next()) {
            section = if header.ends_with(":vars") {
                Section::Vars
            } else if header.ends_with(":children") {
                Section::Skip
            } else {
                Section::Hosts
            };
            continue;
        }
        if section == Section::Skip {
            continue;
        }
        // Offset of `trimmed` within the document, so value spans are absolute.
        let indent = line.len() - line.trim_start().len();
        let base = start + indent;
        if section == Section::Vars {
            if let Some(v) = pair(trimmed, base) {
                if !GroupPosition::Yes.consumes(&v.name) {
                    out.push(v);
                }
            }
            continue;
        }
        // A host line: the first token is the host name, the rest are `k=v` pairs.
        for (i, (tok_at, tok_end)) in host_tokens(trimmed, base).into_iter().enumerate() {
            if i == 0 {
                continue;
            }
            let tok = &text[tok_at..tok_end];
            if let Some(v) = pair(tok, tok_at) {
                out.push(v);
            }
        }
    }
    out
}

/// Does this line carry a section tag ansible rejects? A tag is only read off a line that
/// matches the section pattern in full (`ini.py::_compile_patterns`): `[name:tag]` with a
/// non-empty, whitespace-free name, a `\w+` tag, and nothing after the `]` but whitespace
/// or a `#` comment. `[web:hosts]` is explicit-but-valid — measured, the host and its vars
/// arrive. A line that misses the pattern is not a section at all and is not this rule's.
fn unknown_section_type(line: &str) -> bool {
    let Some(rest) = line.strip_prefix('[') else { return false };
    let Some(end) = rest.find(']') else { return false };
    let after = rest[end + 1..].trim_start();
    if !after.is_empty() && !after.starts_with('#') {
        return false;
    }
    let Some((name, tag)) = rest[..end].split_once(':') else { return false };
    if name.is_empty()
        || name.contains(char::is_whitespace)
        || tag.is_empty()
        || !tag.chars().all(|c| c.is_alphanumeric() || c == '_')
    {
        return false;
    }
    !matches!(tag, "hosts" | "children" | "vars")
}

/// Host-line tokens, as byte ranges into the document.
///
/// Ansible splits these with `shlex.split(line, comments=True)` (`ini.py:316`), so quotes
/// group a value containing spaces and an unquoted `#` opens a comment anywhere in the line
/// — with no space needed before it. Splitting on whitespace instead truncated
/// `var="hello world"` at the space and showed `"hello` in a hover, and left the tail of
/// `x=1#note` in the value.
fn host_tokens(line: &str, base: usize) -> Vec<(usize, usize)> {
    let cut = comment_cut(line);
    let line = &line[..cut];
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    let mut quote: Option<char> = None;
    for (i, c) in line.char_indices() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None if c == '"' || c == '\'' => {
                start.get_or_insert(i);
                quote = Some(c);
            }
            None if c.is_whitespace() => {
                if let Some(s) = start.take() {
                    out.push((base + s, base + i));
                }
            }
            None => {
                start.get_or_insert(i);
            }
        }
    }
    if let Some(s) = start {
        out.push((base + s, base + line.len()));
    }
    out
}

/// Where an unquoted `#` opens a comment, or the end of the line.
fn comment_cut(line: &str) -> usize {
    let mut quote: Option<char> = None;
    for (i, c) in line.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '#' => return i,
            None if c == '"' || c == '\'' => quote = Some(c),
            None => {}
        }
    }
    line.len()
}

/// One `name=value`, with the span covering the value. `None` when there is no `=`, which
/// on a host line is a connection token rather than a variable.
///
/// Whitespace and one layer of matching quotes are trimmed off the value, so hover shows
/// `hello world` and `5` rather than `"hello world"` and ` 5` — matching what ansible
/// resolves the value to, and what the YAML and TOML readers hand back.
///
/// Not replicated: `ini.py::_parse_value` runs the value through `ast.literal_eval`, so a
/// `#` tail is swallowed as a Python comment while a `;` tail survives. Showing the source
/// text as written is the honest reading of an edge that surprising.
fn pair(s: &str, base: usize) -> Option<InventoryVar> {
    let eq = s.find('=')?;
    let name = s[..eq].trim();
    if name.is_empty() || name.contains(char::is_whitespace) {
        return None;
    }
    let raw = &s[eq + 1..];
    let mut start = eq + 1 + (raw.len() - raw.trim_start().len());
    let mut end = s.len() - (raw.len() - raw.trim_end().len());
    let inner = s.get(start..end).unwrap_or("");
    if inner.len() >= 2
        && ((inner.starts_with('"') && inner.ends_with('"'))
            || (inner.starts_with('\'') && inner.ends_with('\'')))
    {
        start += 1;
        end -= 1;
    }
    Some(InventoryVar { name: name.to_string(), span: Span { start: base + start, end: base + end } })
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
            .map(|p| {
                // `sources` joins with the host separator, so on Windows the same walk comes
                // back `inv\a.ini`. Normalize here, not in each assertion.
                p.strip_prefix("/p").unwrap_or(p).to_string_lossy().replace('\\', "/")
            })
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
        // demo/tasks/lenient_scalar.yml: a Jinja list inside a folded scalar, indented, in
        // a file that is a task list. It was offered as an inventory.
        // demo/tasks/lenient_scalar.yml, and the same shape in a mapping so the column-0
        // rule is what rejects it rather than the file happening to be a sequence. Both
        // were offered as inventories until section headers had to start at column 0.
        assert!(
            !sniff(
                "lenient_scalar.yml",
                "- name: build argv\n  ansible.builtin.set_fact:\n    a: \"{{\n        [_beacon_mode]\n      }}\"\n"
            ),
            "a bracketed jinja list in a task file"
        );
        assert!(
            !sniff("group_vars_all.yml", "argv: \"{{\n    [mode]\n  }}\"\n"),
            "a bracketed jinja list in a mapping"
        );
        assert!(!sniff(".hidden.ini", "[webservers]\nweb01\n"), "a dotfile");
    }

    /// A `Cargo.toml` is not a host list, and the generic sniff said it was.
    ///
    /// Every TOML `[table]` satisfies "a line starting with `[` and ending with `]`", so the
    /// INI test accepted *any* TOML file. The picker offered this repo's own four manifests
    /// as candidate inventories, with `[package]` and `[dependencies]` reading as groups.
    ///
    /// Asserted against the real `Cargo.toml` rather than a snippet: a snippet keeps passing
    /// once someone rewrites the manifest, and the manifest is the file that was actually
    /// being offered. The positive case is the control — narrowing the rule until nothing
    /// matches would satisfy the negative on its own.
    #[test]
    fn a_cargo_manifest_is_not_an_inventory_but_a_toml_host_list_is() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let text = std::fs::read_to_string(&manifest).expect("the crate's own manifest");
        let nodes = Document::new(text.clone()).parse().unwrap_or_default();
        assert!(
            !looks_like_inventory(&manifest, &text, &nodes),
            "a cargo manifest was offered as an inventory"
        );

        let inv = concat!(
            "[web.vars]\n",
            "deploy_env = \"prod\"\n",
            "[web.hosts.web01]\n",
            "ip = \"10.0.0.1\"\n",
        );
        let nodes = Document::new(inv.to_string()).parse().unwrap_or_default();
        assert!(
            looks_like_inventory(Path::new("inv.toml"), inv, &nodes),
            "a real toml host list must still be offered"
        );
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

    /// `[g:children]` is neither a vars section nor a host list, and it used to share a
    /// `false` flag with the host-line reader — so a malformed entry was read as a host
    /// line and its `k=v` became a variable. Ansible accepts only group names there, so
    /// the honest response to that input is to ignore it, not to invent a definition.
    #[test]
    fn a_children_section_contributes_nothing_even_when_malformed() {
        let src = concat!(
            "[webservers:children]\n",
            "leafs not_a_var=oops\n",
            "[webservers:vars]\n",
            "real_var=yes\n",
        );
        let got = ini_vars(src);
        assert_eq!(
            names(&got),
            ["real_var"],
            "a `:children` line contributed a variable"
        );
    }

    /// JSON is a static inventory too — measured, `-i inv.json` and the same content with
    /// no extension both reach the play. It is valid YAML, so this asks whether it survives
    /// OUR parser rather than assuming the equivalence holds all the way through.
    #[test]
    fn a_json_inventory_reads_as_yaml() {
        let src = concat!(
            r#"{"all": {"children": {"webservers": {"#,
            r#""hosts": {"web01": {"json_host_var": "FROM_JSON"}},"#,
            r#""vars": {"json_group_var": "FROM_JSON_GROUP"}}}}}"#,
        );
        let nodes = Document::new(src.to_string()).parse().expect("json parses as yaml");
        assert_eq!(classify(Path::new("inv.json"), src, &nodes, &crate::testing::MemFs::new(&[])), Kind::Yaml);
        let got = yaml_vars(&nodes);
        assert_eq!(names(&got), ["json_host_var", "json_group_var"]);
        // Real byte offsets into the file, not a path expression: hover prints this text
        // and go-to-definition lands on it. Asserting the slice rather than just the name,
        // because a reader that finds the right names and points at the wrong place is
        // exactly the confident-wrong-hover this project treats as worse than silence.
        let h = got.iter().find(|v| v.name == "json_host_var").unwrap();
        assert_eq!(h.span.slice(src), "FROM_JSON");
        let g = got.iter().find(|v| v.name == "json_group_var").unwrap();
        assert_eq!(g.span.slice(src), "FROM_JSON_GROUP");
        // And the picker offers it, which is a different predicate from parsing it.
        assert!(looks_like_inventory(Path::new("inv.json"), src, &nodes));
    }

    /// Every row of the range table on [`expand_host_pattern`], measured with
    /// `ansible-inventory --list` before any of this was written.
    ///
    /// This is the piece a "no such host" rule cannot get wrong: under-expanding invents a
    /// missing host out of a correct file, which is a false ERROR on working code.
    #[test]
    fn host_patterns_expand_the_way_ansible_expands_them() {
        let e = |p: &str| expand_host_pattern(p).expect("within the cap");
        assert_eq!(e("web[01:05]"), ["web01", "web02", "web03", "web04", "web05"]);
        assert_eq!(e("db[1:3]"), ["db1", "db2", "db3"], "padding not asked for, none given");
        assert_eq!(e("rack[a:c]"), ["racka", "rackb", "rackc"]);
        assert_eq!(e("s[00:10:5]"), ["s00", "s05", "s10"], "step, both ends inclusive");
        assert_eq!(e("web[01:02].example.com"), ["web01.example.com", "web02.example.com"]);
        assert_eq!(e("r[1:2]-n[a:b]"), ["r1-na", "r1-nb", "r2-na", "r2-nb"], "cartesian");
        assert_eq!(e("solo"), ["solo"], "a plain name is one host");

        // A bracket that is not a range stays literal rather than expanding to nothing —
        // losing the name would be the direction that invents a missing host.
        assert_eq!(e("web[01"), ["web[01"]);
        assert_eq!(e("web[]"), ["web[]"]);
        assert_eq!(e("web[a:b:c]"), ["web[a:b:c]"], "unparseable step");

        // Uppercase, and the mixed-case case that ASCII ordering gets wrong. Measured:
        // `web[a:C]` is 29 hosts, because the endpoints index `ascii_letters` — the whole
        // lowercase run, then A, B, C. Comparing bytes gives an empty range, which would
        // make all 29 read as "no such host".
        assert_eq!(e("web[A:C]"), ["webA", "webB", "webC"]);
        let mixed = e("web[a:C]");
        assert_eq!(mixed.len(), 29, "{mixed:?}");
        assert_eq!(mixed[0], "weba");
        assert_eq!(mixed[25], "webz", "the lowercase run ends at index 25");
        assert_eq!(&mixed[26..], ["webA", "webB", "webC"]);

        // Reversed is empty, matching ansible — `web[5:1]` contributed no host, measured.
        assert!(e("web[5:1]").is_empty());
        assert!(e("web[c:a]").is_empty());

        // Over the cap the answer is "I cannot enumerate this", never a truncated list: a
        // short list would report every host past the cut as a typo. `web[1:1000000]` built
        // a million strings in 360ms before this bound existed.
        assert!(expand_host_pattern("web[1:1000000]").is_none());
        assert!(
            expand_host_pattern("a[1:100]b[1:100]c[1:100]").is_none(),
            "nested brackets multiply, so the cap has to see the product"
        );
        assert!(expand_host_pattern("web[1:65536]").is_some(), "a large but real fleet");
    }

    /// The host lists each reader returns, with the negatives that matter: a group name is
    /// not a host, and neither is anything in a `:vars` or `:children` section.
    ///
    /// Measured control for the group half — `'num' in hostvars` is **False** for a group
    /// called `num`, and `hostvars['num']` fails with the same bare message an unknown host
    /// gives. So a reader that returned group names would silence the rule on real typos.
    #[test]
    fn the_readers_return_hosts_and_not_group_names() {
        let ini = concat!(
            "ungrouped_host\n",
            "[web]\n",
            "web[01:02] host_line_ip=10.0.0.1\n",
            "[web:vars]\n",
            "ntp=1\n",
            "[web:children]\n",
            "leafs\n",
        );
        assert_eq!(ini_hosts(ini).unwrap(), ["ungrouped_host", "web01", "web02"]);

        let yaml = concat!(
            "all:\n",
            "  children:\n",
            "    web:\n",
            "      hosts:\n",
            "        node[01:03]:\n",
            "        plain1:\n",
            "      vars:\n",
            "        ntp: 1\n",
        );
        let nodes = Document::new(yaml.to_string()).parse().unwrap();
        assert_eq!(yaml_hosts(&nodes).unwrap(), ["node01", "node02", "node03", "plain1"]);

        let toml = "[web.hosts.web01]\nip = \"1\"\n[web.vars]\nntp = 1\n";
        assert_eq!(toml_hosts(toml).unwrap(), ["web01"]);

        // A trailing comment is not a host, and neither is anything after the first token.
        assert_eq!(ini_hosts("node1  # a comment\n").unwrap(), ["node1"]);
        assert_eq!(ini_hosts("node1 ansible_host=10.0.0.1 x=2\n").unwrap(), ["node1"]);
        assert_eq!(ini_hosts("; leading semicolon comment\nnode1\n").unwrap(), ["node1"]);

        // The implicit ungrouped section is where a file starts, so a host before any header
        // counts — and a `[group]` header itself is never a host.
        assert_eq!(ini_hosts("first\n[g]\nsecond\n").unwrap(), ["first", "second"]);

        // A YAML group with an empty or absent `hosts:` contributes none, and must not
        // panic on the null.
        let empty = Document::new("all:\n  children:\n    web:\n      hosts:\n".to_string())
            .parse()
            .unwrap();
        assert!(yaml_hosts(&empty).unwrap().is_empty());
        let novars = Document::new("all:\n  vars:\n    x: 1\n".to_string()).parse().unwrap();
        assert!(yaml_hosts(&novars).unwrap().is_empty(), "a vars-only file declares no host");

        // TOML with hosts and no vars table, and the invalid-TOML silence.
        assert_eq!(toml_hosts("[web.hosts.only1]\n").unwrap(), ["only1"]);
        assert!(toml_hosts("[unclosed\nx = ").unwrap().is_empty());

        // The same file-wide rule `ini_vars` follows: an unknown section tag makes ansible
        // discard the whole file, so it can contribute no hosts either. `:hosts` is *valid*
        // and is the control — measured, `[web:hosts]` really does put `node1` in the
        // inventory, so only the `:var` typo may empty the list.
        assert!(ini_hosts("[web:var]\nnode1\n").unwrap().is_empty(), "a typo'd tag discards it");
        assert_eq!(ini_hosts("[web:hosts]\nnode1\n").unwrap(), ["node1"], "`:hosts` is a real tag");
    }

    /// The measured shapes, from `ansible-inventory -i inv.toml --list`: a `[group.vars]`
    /// table and a `[group.hosts.<name>]` table both reach the play.
    ///
    /// The negatives carry the weight. `children` is a LIST of group names here — unlike
    /// YAML, where it nests groups — so nothing in it is a variable; and the grammar TOML
    /// allows but a line reader would trip on (multi-line strings, arrays, inline tables)
    /// has to come out right, since ansible hands the whole file to `tomllib` and accepts
    /// all of it.
    #[test]
    fn toml_reads_group_and_host_tables_and_nothing_else() {
        let src = concat!(
            "[webservers.vars]\n",
            "ntp_server = \"10.0.0.1\"\n",
            "motd = \"\"\"\n",
            "a = not_a_variable\n",
            "\"\"\"\n",
            "ports = [\n",
            "  80,\n",
            "  443,\n",
            "]\n",
            "limits = { soft = 1, hard = 2 }\n",
            "children = [\"leafs\", \"spines\"]\n",
            "\n",
            "[webservers.hosts.web01]\n",
            "host_line_ip = \"10.0.0.11\"\n",
        );
        let got = toml_vars(src);
        assert_eq!(
            names(&got),
            ["ntp_server", "motd", "ports", "limits", "children", "host_line_ip"],
            "a continuation line was read as a variable, or a real one was lost"
        );
        // Spans point at the value, quotes trimmed, so hover matches the other two readers.
        let v = got.iter().find(|v| v.name == "ntp_server").unwrap();
        assert_eq!(v.span.slice(src), "10.0.0.1");
        let h = got.iter().find(|v| v.name == "host_line_ip").unwrap();
        assert_eq!(h.span.slice(src), "10.0.0.11");

        // `children` under a GROUP is a list of group names, and contributes no variables.
        let group_children = "[webservers]\nchildren = [\"leafs\"]\n[webservers.vars]\nx = 1\n";
        assert_eq!(names(&toml_vars(group_children)), ["x"]);

        // Invalid TOML is silence, not a guess.
        assert!(toml_vars("[unclosed\nx = ").is_empty());
    }

    #[test]
    fn a_toml_inventory_is_named_rather_than_read_as_ini() {
        let src = concat!(
            "[webservers.vars]\n",
            "ntp_server = \"10.0.0.1\"\n",
            "[webservers.hosts.web01]\n",
            "host_line_ip = \"10.0.0.11\"\n",
        );
        let nodes = Document::new(src.to_string()).parse().unwrap_or_default();
        let fs = crate::testing::MemFs::new(&[]);
        assert_eq!(classify(Path::new("inv.toml"), src, &nodes, &fs), Kind::Toml);
        assert_eq!(classify(Path::new("INV.TOML"), src, &nodes, &fs), Kind::Toml, "case");
        // The same content under a name ansible would hand to the ini plugin still is INI.
        assert_eq!(classify(Path::new("hosts.ini"), src, &nodes, &fs), Kind::Ini);
    }

    /// Host lines are `shlex.split(line, comments=True)`, not whitespace-split. Every
    /// expectation below is the value `ansible-inventory --list` reported for this exact
    /// file — the whitespace reader got three of the six wrong, and a wrong value in a
    /// hover is the failure this project exists to avoid.
    #[test]
    fn ini_values_match_what_ansible_resolves() {
        let src = concat!(
            "[web]\n",
            "node1 quoted=\"hello world\" after=2\n",
            "node2 inline=1 # a trailing comment\n",
            "node3 hashy=1#nospace\n",
            "[web:vars]\n",
            "spaced = 5\n",
            "c = \"quoted value\"\n",
        );
        let got = ini_vars(src);
        let seen: Vec<(String, &str)> =
            got.iter().map(|v| (v.name.clone(), v.span.slice(src))).collect();
        assert_eq!(
            seen,
            vec![
                // `"hello world"` is ONE token; whitespace-splitting truncated it to `"hello`.
                ("quoted".to_string(), "hello world"),
                ("after".to_string(), "2"),
                ("inline".to_string(), "1"),
                // `#` opens a comment with no space before it.
                ("hashy".to_string(), "1"),
                // A `:vars` line is split on the first `=` and both sides stripped.
                ("spaced".to_string(), "5"),
                ("c".to_string(), "quoted value"),
            ]
        );
    }

    /// The corpus shape: `all:` with `vars:`, and hosts nested under `children:`.
    #[test]
    fn yaml_reads_group_vars_and_nested_host_vars() {
        let src = concat!(
            "all:\n",
            "  vars:\n",
            "    app_version: 2.6\n",
            "  children:\n",
            "    app_servers:\n",
            "      hosts:\n",
            "        server1:\n",
            "          infiniband_ip: 192.168.100.4\n",
            "        server2:\n",
            "          infiniband_ip: 192.168.100.5\n",
        );
        let nodes = Document::new(src.to_string()).parse().expect("valid yaml");
        let got = yaml_vars(&nodes);
        assert_eq!(names(&got), ["app_version", "infiniband_ip", "infiniband_ip"]);
        assert_eq!(got[1].span.slice(src), "192.168.100.4");
        // Group and host names are structure, not variables.
        assert!(!names(&got).contains(&"app_servers"));
        assert!(!names(&got).contains(&"server1"));
    }

    /// A section type that is not `vars` or `children` kills the WHOLE file, not the section.
    ///
    /// Measured on 2.21.2 against this exact text: `Section [web:var] has unknown type: var`,
    /// then `Unable to parse ... as an inventory source` and `No inventory was parsed`. The
    /// control is `real_var`, which sits in a perfectly good `[web]` section *above* the typo
    /// and is still gone — `hostvars` comes back empty. So the rule is file-wide.
    ///
    #[test]
    fn an_unknown_section_type_discards_the_whole_ini_file() {
        let src = concat!(
            "[web]\n",
            "node1 real_var=CONTROL\n",
            "\n",
            "[web:var]\n",
            "typo_var=2\n",
            "alsotyped=3 second=4\n",
        );
        // Ansible parsed none of it, so neither may we — including the valid section.
        assert_eq!(names(&ini_vars(src)), Vec::<&str>::new());
    }

    /// The control for the rule above, so the detector could not pass by killing every
    /// tagged section: `hosts` is the third *valid* tag (`ini.py:189`) — measured, the
    /// host line's vars arrive — and a tag is only read off a full section match, so a
    /// bracketed jinja list like `[x:y]` inside a value must not condemn the file.
    #[test]
    fn a_valid_or_non_section_tag_does_not_discard_the_file() {
        assert_eq!(names(&ini_vars("[web:hosts]\nnode1 tagged_var=YES\n")), ["tagged_var"]);
        assert_eq!(
            names(&ini_vars("[web:vars]\nreal_var={{ a[b:c] }}\n")),
            ["real_var"],
            "a sliced jinja value is not a section header"
        );
    }

    /// A host with no variables is a null value, and must not panic or invent a name.
    #[test]
    fn a_bare_host_contributes_nothing() {
        let nodes = Document::new("all:\n  hosts:\n    node1:\n".to_string()).parse().unwrap();
        assert!(yaml_vars(&nodes).is_empty());
    }

    /// `vars`/`hosts`/`children` are only meaningful *inside* a group. Measured on 2.21.2
    /// against this exact file: ansible defines `real_var` and nothing else, warning
    /// "Skipping unexpected key (bogus) in group (all)" and treating top-level `vars:` as a
    /// group named `vars` — it appears in `all`'s children.
    ///
    /// Reading them by name at any depth invents variables from a typo (`var:` for `vars:`),
    /// which is the one failure that matters here: a name we invent is a name `var-undefined`
    /// then stops reporting.
    #[test]
    fn yaml_section_names_are_read_only_where_a_group_can_have_them() {
        let src = concat!(
            "all:\n",
            "  hosts:\n",
            "    web01:\n",
            "      real_var: CONTROL\n",
            "  bogus:\n",
            "    vars:\n",
            "      invented: NESTED_UNEXPECTED\n",
            "vars:\n",
            "  toplevel_invented: TOP_LEVEL_VARS\n",
        );
        let nodes = Document::new(src.to_string()).parse().expect("valid yaml");
        assert_eq!(names(&yaml_vars(&nodes)), ["real_var"]);
    }

    /// `ansible_group_priority` is a merge-order control rather than a variable — but only
    /// where it is written. Measured on core 2.21.3 with `ansible-inventory --host node1`
    /// over thirteen fixtures covering every position these three readers walk, each
    /// carrying an ordinary `control` variable in the *same* position so that an absence is
    /// an absence and not a fixture that was never read:
    ///
    /// | position                                             | ansible defines it |
    /// | ---------------------------------------------------- | ------------------ |
    /// | `[g:vars]`, `vars:`, `[g.vars]` — any group, any depth | **no**            |
    /// | host line, `hosts:` entry, `[g.hosts.h]` — any host   | **yes**, `10`      |
    ///
    /// `Group.set_variable` consumes it (`inventory/group.py:216-217`); `Host.set_variable`
    /// (`inventory/host.py:119-130`) has no such branch and stores it like any other key.
    /// So the drop is scoped to the group position: dropping it from the host position too
    /// would delete a variable that really exists, trading "hover points at a non-variable"
    /// for "hover says a real variable is never defined" (T-178).
    ///
    /// `all` and a parent group are not special — they are group positions like any other,
    /// which is why one gate per reader covers every one of them.
    #[test]
    fn ini_vars_drops_group_priority_from_every_vars_section() {
        let src = concat!(
            "[web]\n",
            "node1\n",
            "\n",
            "[web:vars]\n",
            "ansible_group_priority=10\n",
            "group_var=FROM_GROUP\n",
            "\n",
            "[all:vars]\n",
            "ansible_group_priority=20\n",
            "all_var=FROM_ALL\n",
        );
        assert_eq!(names(&ini_vars(src)), ["group_var", "all_var"]);
    }

    /// The control for the test above: the same key on a host line **is** a variable, and
    /// the fix T-178 originally proposed — a reader-wide drop — fails right here.
    #[test]
    fn ini_vars_keeps_group_priority_on_a_host_line() {
        let src = concat!(
            "node1 ansible_group_priority=10 ungrouped_var=FROM_UNGROUPED\n",
            "[web]\n",
            "node2 ansible_group_priority=20 host_var=FROM_HOST\n",
        );
        assert_eq!(
            names(&ini_vars(src)),
            ["ansible_group_priority", "ungrouped_var", "ansible_group_priority", "host_var"]
        );
    }

    /// Nesting does not change the answer: a `vars:` under `children:` is still a group
    /// position. The recursion reuses one match arm, so one gate covers every depth.
    #[test]
    fn yaml_vars_drops_group_priority_from_group_vars_at_any_depth() {
        let src = concat!(
            "parent:\n",
            "  vars:\n",
            "    ansible_group_priority: 10\n",
            "    parent_var: FROM_PARENT\n",
            "  children:\n",
            "    web:\n",
            "      hosts:\n",
            "        node1:\n",
            "      vars:\n",
            "        ansible_group_priority: 20\n",
            "        child_var: FROM_CHILD\n",
        );
        let nodes = Document::new(src.to_string()).parse().expect("valid yaml");
        assert_eq!(names(&yaml_vars(&nodes)), ["parent_var", "child_var"]);
    }

    /// The control: a `hosts:` entry keeps it, nested or not.
    #[test]
    fn yaml_vars_keeps_group_priority_on_a_host_entry() {
        let src = concat!(
            "parent:\n",
            "  children:\n",
            "    web:\n",
            "      hosts:\n",
            "        node1:\n",
            "          ansible_group_priority: 10\n",
            "          host_var: FROM_HOST\n",
        );
        let nodes = Document::new(src.to_string()).parse().expect("valid yaml");
        assert_eq!(names(&yaml_vars(&nodes)), ["ansible_group_priority", "host_var"]);
    }

    #[test]
    fn toml_vars_drops_group_priority_from_a_vars_table() {
        let src = concat!(
            "[web.vars]\n",
            "ansible_group_priority = 10\n",
            "group_var = \"FROM_GROUP\"\n",
            "[web.hosts.node1]\n",
            "[all.vars]\n",
            "ansible_group_priority = 20\n",
            "all_var = \"FROM_ALL\"\n",
        );
        assert_eq!(names(&toml_vars(src)), ["group_var", "all_var"]);
    }

    /// The control: a `[g.hosts.h]` table keeps it.
    #[test]
    fn toml_vars_keeps_group_priority_in_a_host_table() {
        let src = concat!(
            "[web.hosts.node1]\n",
            "ansible_group_priority = 10\n",
            "host_var = \"FROM_HOST\"\n",
        );
        assert_eq!(names(&toml_vars(src)), ["ansible_group_priority", "host_var"]);
    }
}
