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

/// A ticket file: which folder it sits in, and the status its own header table claims.
struct Ticket {
    at: Where,
    file: String,
    status: String,
}

/// A row in one of the README tables: which section it appears under, and where its link
/// points.
struct Row {
    section: Where,
    link: String,
}

/// `| open   | P2       | S    | — |` -> `open`. The status is the first cell of the row
/// after the header separator; `**rejected**` loses its emphasis so it compares as a word.
fn status_of(text: &str) -> String {
    let mut rows = text
        .lines()
        .filter(|l| l.trim_start().starts_with('|'))
        .skip(2);
    let cell = rows
        .next()
        .and_then(|l| l.split('|').nth(1))
        .unwrap_or_default();
    cell.trim().trim_matches('*').to_string()
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
            out.insert(
                id.to_string(),
                Ticket { at, file, status: status_of(&text) },
            );
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
    for line in text.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            section = match heading.trim() {
                "Open" => Some(Where::Open),
                "Closed" => Some(Where::Closed),
                _ => None,
            };
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
        out.entry(id.to_string()).or_default().push(Row { section, link });
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
