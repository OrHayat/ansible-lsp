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
    priority: String,
    size: String,
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

/// `| open   | P2       | S    | — |` -> ("open", "P2", "S"). The cells after the header
/// separator; `**rejected**` loses its emphasis so it compares as a word.
fn header_of(text: &str) -> (String, String, String) {
    let cells: Vec<String> = text
        .lines()
        .filter(|l| l.trim_start().starts_with('|'))
        .nth(2)
        .map(|l| l.split('|').map(|c| c.trim().trim_matches('*').to_string()).collect())
        .unwrap_or_default();
    let get = |i: usize| cells.get(i).cloned().unwrap_or_default();
    (get(1), get(2), get(3))
}

fn tickets() -> BTreeMap<String, Ticket> {
    let mut out = BTreeMap::new();
    for at in [Where::Open, Where::Closed] {
        let dir = tasks_dir().join(at.dir());
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let file = path.file_name().unwrap().to_string_lossy().to_string();
            let Some(id) = file.get(..5).filter(|s| s.starts_with("T-")) else { continue };
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let (status, priority, size) = header_of(&text);
            out.insert(id.to_string(), Ticket { at, file, status, priority, size });
        }
    }
    out
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
