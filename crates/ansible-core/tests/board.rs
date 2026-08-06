//! The board must agree with the folders (T-081).
//!
//! `tasks/README.md` says "Status is the folder", but the tables everyone reads are
//! hand-maintained, and closing a ticket is three manual edits — `git mv`, flip the status
//! line, move the README row. Only the first has a failure mode, which is why 27 of the
//! other two had been skipped when this was written.
//!
//! This is the check half of T-081. It writes nothing, so no generator can eat the prose
//! sections, and it fails loudly enough that the row can't be forgotten again.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn tasks_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tasks")
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum Where {
    Open,
    Closed,
}

impl Where {
    fn dir(self) -> &'static str {
        match self {
            Where::Open => "open",
            Where::Closed => "closed",
        }
    }
}

/// A ticket file: which folder it sits in, and what its own header table claims.
struct Ticket {
    at: Where,
    file: String,
    status: String,
    kind: String,
    priority: String,
    size: String,
    /// The `Epic` column — the child's half of the epic link. Empty for most tickets.
    epic: String,
    /// The `## Children` checklist — the epic's half. `(id, ticked)`, in file order.
    children: Vec<(String, bool)>,
}

/// A row in one of the README tables: which section it appears under, where its link
/// points, and the cells that duplicate the ticket header. `priority` is the `### P1`-style
/// subsection (None under Downstream); `size`/`outcome` are the third cell, which is Size
/// in Open tables and Outcome in the Closed one.
struct Row {
    section: Where,
    link: String,
    priority: Option<String>,
    size: Option<String>,
    outcome: Option<String>,
}

/// `| open | bug | P2 | S | — |` -> ("open", "P2", "S"). Columns are matched by header
/// name, not position: `Kind` and `Epic` were added after most of these tickets were
/// written, and a positional read would take `Kind` for the priority. A ticket with no
/// such column yields `""`, which only the status check can trip on.
/// `**rejected**` loses its emphasis so it compares as a word.
fn field(text: &str, name: &str) -> String {
    let row = |n: usize| -> Vec<String> {
        text.lines()
            .filter(|l| l.trim_start().starts_with('|'))
            .nth(n)
            .map(|l| l.split('|').map(|c| c.trim().trim_matches('*').to_string()).collect())
            .unwrap_or_default()
    };
    let (head, values) = (row(0), row(2));
    head.iter()
        .position(|h| h == name)
        .and_then(|i| values.get(i).cloned())
        .unwrap_or_default()
}

/// The `- [ ] T-0NN — Title` lines under an epic's `## Children` heading, as
/// `(id, ticked)`. Empty for every ticket that isn't an epic.
fn children_of(text: &str) -> Vec<(String, bool)> {
    text.lines()
        .skip_while(|l| !l.starts_with("## Children"))
        .skip(1)
        .take_while(|l| !l.starts_with("## "))
        .filter_map(|l| {
            let ticked = l.starts_with("- [x]");
            if !ticked && !l.starts_with("- [ ]") {
                return None;
            }
            let id = l.get(6..11)?;
            id.starts_with("T-")
                .then(|| (id.to_string(), ticked))
        })
        .collect()
}

/// Every ticket file on disk, in folder order, **including** two that claim the same id.
/// Keyed collections lose the duplicate silently, so the list is what gets collected and
/// `one_file_per_ticket_id` runs over it before anything else trusts the map.
fn ticket_files() -> Vec<(String, Ticket)> {
    let mut out = Vec::new();
    for at in [Where::Open, Where::Closed] {
        let dir = tasks_dir().join(at.dir());
        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .collect();
        entries.sort();
        for path in entries {
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let file = path.file_name().unwrap().to_string_lossy().to_string();
            let Some(id) = file.get(..5).filter(|s| s.starts_with("T-")) else { continue };
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let kind = field(&text, "Kind");
            out.push((id.to_string(), Ticket {
                at,
                file,
                status: field(&text, "Status"),
                // No Kind column == written before kinds existed == a plain task.
                kind: if kind.is_empty() { "task".into() } else { kind },
                priority: field(&text, "Priority"),
                size: field(&text, "Size"),
                epic: field(&text, "Epic"),
                children: children_of(&text),
            }));
        }
    }
    out
}

fn tickets() -> BTreeMap<String, Ticket> {
    ticket_files().into_iter().collect()
}

/// Two files claiming one id is the one failure the rest of this file cannot see: every
/// other check reads a map keyed by id, and a map keeps exactly one of them. The board CLI
/// never creates this — it derives the filename from the title — but hand-writing a ticket
/// file, or guessing a name the CLI truncated (`slug` caps at 60 chars), does.
///
/// Silently keeping one means the other's contents are invisible: no README row is demanded
/// for it, its epic link is unchecked, and it will not be found by `board show`.
#[test]
fn one_file_per_ticket_id() {
    let mut by_id: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (id, t) in ticket_files() {
        by_id.entry(id).or_default().push(format!("tasks/{}/{}", t.at.dir(), t.file));
    }
    let dupes: Vec<String> = by_id
        .iter()
        .filter(|(_, files)| files.len() > 1)
        .map(|(id, files)| format!("{id}  {}", files.join("\n       ")))
        .collect();

    assert!(
        dupes.is_empty(),
        "{} ticket id{} claimed by more than one file. Every other check here reads a map \
         keyed by id and would silently ignore all but one — delete or rename the extras:\n  {}",
        dupes.len(),
        if dupes.len() == 1 { " is" } else { "s are" },
        dupes.join("\n  ")
    );
}

/// Every `| T-0NN | … |` row in the README, tagged with the `## Open` / `## Closed` section
/// it falls under. `### P1` and friends are subsections of Open and don't reset it.
fn rows() -> BTreeMap<String, Vec<Row>> {
    let text = std::fs::read_to_string(tasks_dir().join("README.md")).unwrap_or_default();
    let mut out: BTreeMap<String, Vec<Row>> = BTreeMap::new();
    let mut section = None;
    let mut sub: Option<String> = None;
    for line in text.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            section = match heading.trim() {
                "Open" => Some(Where::Open),
                "Closed" => Some(Where::Closed),
                _ => None,
            };
            sub = None;
            continue;
        }
        if let Some(heading) = line.strip_prefix("### ") {
            sub = ["P1", "P2", "P3"]
                .iter()
                .find(|p| heading.starts_with(**p))
                .map(|p| p.to_string());
            continue;
        }
        let Some(section) = section else { continue };
        if !line.trim_start().starts_with('|') {
            continue;
        }
        let Some(id) = line.split('|').nth(1).map(str::trim) else { continue };
        if !(id.len() == 5 && id.starts_with("T-") && id[2..].bytes().all(|b| b.is_ascii_digit()))
        {
            continue;
        }
        let link = line
            .split_once("](")
            .and_then(|(_, rest)| rest.split_once(')'))
            .map(|(p, _)| p.to_string())
            .unwrap_or_default();
        let third = line
            .split('|')
            .nth(3)
            .map(|c| c.trim().trim_matches('*').to_string())
            .filter(|c| !c.is_empty());
        let (priority, size, outcome) = match section {
            Where::Open => (sub.clone(), third, None),
            Where::Closed => (None, None, third),
        };
        out.entry(id.to_string()).or_default().push(Row { section, link, priority, size, outcome });
    }
    out
}

/// Closing a ticket is `git mv`, flip the status line, move the README row. Only the first
/// breaks anything on its own; this is the failure mode for the other two.
#[test]
fn board_agrees_with_the_folders() {
    let tickets = tickets();
    let rows = rows();
    let mut problems: Vec<String> = Vec::new();

    for (id, t) in &tickets {
        match rows.get(id) {
            None => problems.push(format!(
                "{id}  in tasks/{}/ but has no README row — add one",
                t.at.dir()
            )),
            Some(rs) if rs.len() > 1 => problems.push(format!(
                "{id}  listed {} times in the README — keep one row",
                rs.len()
            )),
            Some(rs) => {
                let r = &rs[0];
                if r.section != t.at {
                    problems.push(format!(
                        "{id}  file is in tasks/{}/ but the README lists it under ## {:?}",
                        t.at.dir(),
                        r.section
                    ));
                }
                if !r.link.starts_with(&format!("{}/", t.at.dir())) {
                    problems.push(format!(
                        "{id}  README links to `{}` but the file is tasks/{}/{}",
                        r.link,
                        t.at.dir(),
                        t.file
                    ));
                }
                // Size, priority and outcome duplicate the ticket header — the cells that
                // go stale when a header is hand-edited without `board sync` (T-081).
                if let Some(p) = r.priority.as_deref().filter(|p| *p != t.priority) {
                    problems.push(format!(
                        "{id}  sits in the README's {p} table but its header says {} — run `board sync {id}`",
                        t.priority
                    ));
                }
                if let Some(s) = r.size.as_deref().filter(|s| *s != t.size) {
                    problems.push(format!(
                        "{id}  README row says size {s} but the header says {} — run `board sync {id}`",
                        t.size
                    ));
                }
                if let Some(o) = r.outcome.as_deref() {
                    let first = o.split_whitespace().next().unwrap_or_default();
                    if first != t.status {
                        problems.push(format!(
                            "{id}  README outcome `{o}` disagrees with the header status `{}` — run `board sync {id}`",
                            t.status
                        ));
                    }
                }
            }
        }

        // The status line inside the file is the second of the three manual edits.
        // `partly done` is an open ticket that has shipped some of its surface — T-031 and
        // T-032 are both this, and the README prose says so. The folder still governs.
        let want: &[&str] = match t.at {
            Where::Open => &["open", "partly done"],
            Where::Closed => &["done", "rejected"],
        };
        if !want.contains(&t.status.as_str()) {
            problems.push(format!(
                "{id}  tasks/{}/{} says status `{}` — expected {}",
                t.at.dir(),
                t.file,
                t.status,
                want.join(" or ")
            ));
        }
    }

    for id in rows.keys() {
        if !tickets.contains_key(id) {
            problems.push(format!("{id}  has a README row but no ticket file"));
        }
    }

    assert!(
        problems.is_empty(),
        "the board and tasks/{{open,closed}}/ disagree ({} problems).\n\
         Status is the folder — fix tasks/README.md (or the ticket's status line) to match:\n  {}",
        problems.len(),
        problems.join("\n  ")
    );
}

/// The epic link is written in two places — the child's `Epic` column and the parent's
/// `## Children` checklist — so it has two chances to go stale. `board new -e` and
/// `board adopt` write both together; a hand-edit writes one. This is the failure mode for
/// the other.
///
/// The checklist is a convenience, never the truth: state is the folder, exactly as for
/// status. A box that disagrees with the folder is drift, not a second opinion.
#[test]
fn epics_agree_with_their_children() {
    let tickets = tickets();
    let mut problems: Vec<String> = Vec::new();

    for (id, t) in &tickets {
        // --- the child's half -----------------------------------------------------------
        if !t.epic.is_empty() && t.epic != "—" {
            match tickets.get(&t.epic) {
                None => problems.push(format!(
                    "{id}  names epic {}, which has no ticket file",
                    t.epic
                )),
                Some(p) if p.kind != "epic" => problems.push(format!(
                    "{id}  names epic {} but that ticket's kind is `{}`, not `epic`",
                    t.epic, p.kind
                )),
                Some(p) if !p.children.iter().any(|(c, _)| c == id) => problems.push(format!(
                    "{id}  names epic {} but {} has no `- [ ] {id}` line — \
                     run `board adopt {id} -e {}`",
                    t.epic, t.epic, t.epic
                )),
                Some(_) => {}
            }
        }

        // --- the epic's half ------------------------------------------------------------
        if t.kind != "epic" && !t.children.is_empty() {
            problems.push(format!(
                "{id}  has a ## Children list but its kind is `{}` — only epics have children",
                t.kind
            ));
        }
        for (child, ticked) in &t.children {
            match tickets.get(child) {
                None => problems.push(format!(
                    "{id}  lists child {child}, which has no ticket file"
                )),
                Some(c) if c.epic != *id => problems.push(format!(
                    "{id}  lists child {child}, but {child}'s Epic column says `{}` — \
                     the link must name both ways",
                    if c.epic.is_empty() { "—" } else { &c.epic }
                )),
                // The box is derived state; the folder decides.
                Some(c) if *ticked != (c.at == Where::Closed) => problems.push(format!(
                    "{id}  has {child} ticked `[{}]` but the file is in tasks/{}/ — \
                     run `board {} {child}`",
                    if *ticked { 'x' } else { ' ' },
                    c.at.dir(),
                    if *ticked { "reopen" } else { "close" },
                )),
                Some(_) => {}
            }
        }

        // A closed epic over open children is the drift `board close` refuses to create;
        // it can still arrive by hand-editing or by reopening a child.
        if t.kind == "epic" && t.at == Where::Closed && t.status != "rejected" {
            let open: Vec<&str> = t
                .children
                .iter()
                .filter(|(c, _)| tickets.get(c).is_some_and(|c| c.at == Where::Open))
                .map(|(c, _)| c.as_str())
                .collect();
            if !open.is_empty() {
                problems.push(format!(
                    "{id}  is a closed epic with open {}: {}",
                    if open.len() == 1 { "child" } else { "children" },
                    open.join(", ")
                ));
            }
        }
    }

    assert!(
        problems.is_empty(),
        "epics and their children disagree ({} problems).\n\
         The link is two-way — the child's `Epic` column and the epic's `## Children` list \
         must name each other, and a tick must match the folder:\n  {}",
        problems.len(),
        problems.join("\n  ")
    );
}
