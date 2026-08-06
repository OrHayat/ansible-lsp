//! Ticket lifecycle for `tasks/` (T-081).
//!
//! Closing a ticket used to be three manual edits — `git mv`, flip the status line, move
//! the README row — and only the first breaks anything when skipped, which is how 27 rows
//! drifted. These commands do all the edits together; `cargo test` (board.rs) stays the
//! judge. Std-only on purpose: nothing to install on either Windows or macOS.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
board — ticket lifecycle for tasks/

USAGE
  cargo run -p board -- <COMMAND> [OPTIONS]

COMMANDS
  new <TITLE>                     create a ticket in tasks/open/
      -p, --priority <P1|P2|P3>   required
      -s, --size     <S|M|L>      required
      -b, --blocked-by <IDS>      comma-separated: T-020,T-062
      --problem <TEXT>            pre-fill the Problem section
  close <T-0NN>                   move to tasks/closed/, outcome done
      --rejected                  outcome rejected instead
  reopen <T-0NN>                  move back to tasks/open/
  sync <T-0NN>                    re-derive the README row from the ticket's
                                  header table (after editing priority/size/
                                  blocked-by in the file)
  list                            open tickets, one line each
      -p, --priority <P1|P2|P3>   filter
      -s, --size     <S|M|L>      filter
      --unblocked                 only tickets with no open blockers
      --closed                    list closed tickets instead
  show <T-0NN>                    one ticket in full
  help

GLOBAL OPTIONS
  --dir <PATH>                    tasks/ directory (default: this repo's)
  --dry-run                       print what would change, write nothing

EXIT CODES
  0 ok   1 bad usage   2 operation failed";

#[derive(Debug)]
enum Error {
    Usage(String),
    Op(String),
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(out) => {
            print!("{out}");
            ExitCode::SUCCESS
        }
        Err(Error::Usage(m)) => {
            eprintln!("{m}\n\n{USAGE}");
            ExitCode::from(1)
        }
        Err(Error::Op(m)) => {
            eprintln!("error: {m}");
            ExitCode::from(2)
        }
    }
}

fn run(args: &[String]) -> Result<String, Error> {
    let mut dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tasks");
    let mut dry = false;
    let mut rest: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--dir" => {
                dir = PathBuf::from(
                    it.next().ok_or_else(|| Error::Usage("--dir needs a path".into()))?,
                )
            }
            "--dry-run" => dry = true,
            _ => rest.push(a.clone()),
        }
    }
    let board = Board { dir, dry };
    match rest.first().map(String::as_str) {
        Some("new") => board.new_ticket(&rest[1..]),
        Some("close") => board.close(&rest[1..]),
        Some("reopen") => board.reopen(&rest[1..]),
        Some("sync") => board.sync(&rest[1..]),
        Some("list") => board.list(&rest[1..]),
        Some("show") => board.show(&rest[1..]),
        Some("help") | Some("-h") | Some("--help") => Ok(format!("{USAGE}\n")),
        Some(c) => Err(Error::Usage(format!("unknown command `{c}`"))),
        None => Err(Error::Usage("no command given".into())),
    }
}

struct Board {
    dir: PathBuf,
    dry: bool,
}

/// One ticket file's header table plus its identity on disk.
struct Ticket {
    id: String,
    file: String,
    open: bool,
    title: String,
    status: String,
    priority: String,
    size: String,
    depends: String,
}

impl Board {
    // ---- commands ----------------------------------------------------------------------

    fn new_ticket(&self, args: &[String]) -> Result<String, Error> {
        let mut title = None;
        let mut priority = None;
        let mut size = None;
        let mut blocked: Vec<String> = Vec::new();
        let mut problem = None;
        let mut it = args.iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "-p" | "--priority" => priority = it.next().cloned(),
                "-s" | "--size" => size = it.next().cloned(),
                "-b" | "--blocked-by" => {
                    blocked = it
                        .next()
                        .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
                        .unwrap_or_default()
                }
                "--problem" => problem = it.next().cloned(),
                _ if title.is_none() && !a.starts_with('-') => title = Some(a.clone()),
                _ => return Err(Error::Usage(format!("unexpected argument `{a}`"))),
            }
        }
        let title = title.ok_or_else(|| Error::Usage("new needs a title".into()))?;
        let priority = priority.ok_or_else(|| Error::Usage("new needs --priority".into()))?;
        let size = size.ok_or_else(|| Error::Usage("new needs --size".into()))?;
        if !["P1", "P2", "P3"].contains(&priority.as_str()) {
            return Err(Error::Usage(format!("priority must be P1|P2|P3, got `{priority}`")));
        }
        if !["S", "M", "L"].contains(&size.as_str()) {
            return Err(Error::Usage(format!("size must be S|M|L, got `{size}`")));
        }
        for b in &blocked {
            if id_tokens(b).len() != 1 {
                return Err(Error::Usage(format!("`{b}` is not a ticket id (T-0NN)")));
            }
        }

        let id = format!("T-{:03}", self.max_id()? + 1);
        let file = format!("{id}-{}.md", slug(&title));
        let deps = if blocked.is_empty() { "—".to_string() } else { blocked.join(", ") };
        let mut body = format!(
            "# {id} — {title}\n\n{}\n\n## Problem\n",
            header_table(&["Status", "Priority", "Size", "Depends on"], &[
                "open", &priority, &size, &deps,
            ])
        );
        if let Some(p) = &problem {
            body.push_str(&format!("\n{p}\n"));
        }
        body.push_str("\n## Approach\n\n## Done when\n\n- [ ]\n");

        let path = self.dir.join("open").join(&file);
        self.write(&path, &body)?;

        let mut lines = self.readme()?;
        let cell = self.blockers_cell(&blocked);
        let row = [&id, &format!("[{title}](open/{file})"), &size, &cell];
        self.insert_open_row(&mut lines, &priority, &row.map(String::as_str))?;
        self.save_readme(&lines)?;

        Ok(format!(
            "created tasks/open/{file}\nREADME: added row under ### {priority}\n{}",
            if self.dry { "(dry run — nothing written)\n" } else { "" }
        ))
    }

    fn close(&self, args: &[String]) -> Result<String, Error> {
        let (id, flags) = one_id(args)?;
        let rejected = match flags.as_slice() {
            [] => false,
            [f] if f == "--rejected" => true,
            [f, ..] => return Err(Error::Usage(format!("unexpected argument `{f}`"))),
        };
        let t = self.ticket(&id)?;
        if !t.open {
            return Err(Error::Op(format!("{id} is already closed")));
        }
        let outcome = if rejected { "**rejected**" } else { "done" };

        let from = self.dir.join("open").join(&t.file);
        let text = read(&from)?;
        self.write(&self.dir.join("closed").join(&t.file), &set_status(&text, outcome))?;
        self.remove(&from)?;

        let mut lines = self.readme()?;
        let removed = self.take_row(&mut lines, "## Open", &format!("](open/{})", t.file))?;
        let title = cell(&removed, 2).replace("](open/", "](closed/");
        self.append_closed_row(&mut lines, &[&id, &title, outcome])?;
        self.mark_blocker(&mut lines, &id, true);
        self.save_readme(&lines)?;

        Ok(format!(
            "moved tasks/open/{f} -> tasks/closed/\nstatus {} -> {}\nREADME: row moved to ## Closed\n{}",
            t.status,
            outcome.trim_matches('*'),
            if self.dry { "(dry run — nothing written)\n" } else { "" },
            f = t.file,
        ))
    }

    fn reopen(&self, args: &[String]) -> Result<String, Error> {
        let (id, flags) = one_id(args)?;
        if let Some(f) = flags.first() {
            return Err(Error::Usage(format!("unexpected argument `{f}`")));
        }
        let t = self.ticket(&id)?;
        if t.open {
            return Err(Error::Op(format!("{id} is already open")));
        }

        let from = self.dir.join("closed").join(&t.file);
        let text = read(&from)?;
        self.write(&self.dir.join("open").join(&t.file), &set_status(&text, "open"))?;
        self.remove(&from)?;

        let mut lines = self.readme()?;
        let removed = self.take_row(&mut lines, "## Closed", &format!("](closed/{})", t.file))?;
        let title = cell(&removed, 2).replace("](closed/", "](open/");
        let deps = self.blockers_cell(&id_tokens(&t.depends));
        let deps = if deps == "—" && t.depends != "—" { t.depends.clone() } else { deps };
        let row = [id.as_str(), title.as_str(), t.size.as_str(), deps.as_str()];
        self.insert_open_row(&mut lines, &t.priority, &row)?;
        self.mark_blocker(&mut lines, &id, false);
        self.save_readme(&lines)?;

        Ok(format!(
            "moved tasks/closed/{f} -> tasks/open/\nstatus {} -> open\nREADME: row moved to ### {}\n{}",
            t.status,
            t.priority,
            if self.dry { "(dry run — nothing written)\n" } else { "" },
            f = t.file,
        ))
    }

    /// Rebuild the ticket's README row from its own header table. The row's short title is
    /// editorial and survives; everything else — which table, size, blockers, outcome — is
    /// re-derived from the file.
    fn sync(&self, args: &[String]) -> Result<String, Error> {
        let (id, flags) = one_id(args)?;
        if let Some(f) = flags.first() {
            return Err(Error::Usage(format!("unexpected argument `{f}`")));
        }
        let t = self.ticket(&id)?;
        let folder = if t.open { "open" } else { "closed" };

        let mut lines = self.readme()?;
        let needle = format!("/{})", t.file);
        let title = lines
            .iter()
            .position(|l| l.trim_start().starts_with('|') && l.contains(&needle))
            .map(|i| cell(&lines.remove(i), 2))
            .and_then(|c| {
                c.split_once('[')
                    .and_then(|(_, r)| r.split_once("]("))
                    .map(|(t, _)| t.to_string())
            })
            .unwrap_or_else(|| t.title.clone());
        let link = format!("[{title}]({folder}/{})", t.file);

        if t.open {
            let deps = self.blockers_cell(&id_tokens(&t.depends));
            let deps = if deps == "—" && t.depends != "—" { t.depends.clone() } else { deps };
            let row = [id.as_str(), link.as_str(), t.size.as_str(), deps.as_str()];
            self.insert_open_row(&mut lines, &t.priority, &row)?;
        } else {
            let outcome = if t.status == "rejected" { "**rejected**".into() } else { t.status.clone() };
            self.append_closed_row(&mut lines, &[&id, &link, &outcome])?;
        }
        self.save_readme(&lines)?;

        Ok(format!(
            "README: {id} row rebuilt under {}\n{}",
            if t.open { format!("### {}", t.priority) } else { "## Closed".into() },
            if self.dry { "(dry run — nothing written)\n" } else { "" }
        ))
    }

    fn list(&self, args: &[String]) -> Result<String, Error> {
        let mut priority = None;
        let mut size = None;
        let mut unblocked = false;
        let mut closed = false;
        let mut it = args.iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "-p" | "--priority" => priority = it.next().cloned(),
                "-s" | "--size" => size = it.next().cloned(),
                "--unblocked" => unblocked = true,
                "--closed" => closed = true,
                _ => return Err(Error::Usage(format!("unexpected argument `{a}`"))),
            }
        }
        let all = self.tickets()?;
        let mut out = String::new();
        for t in &all {
            if t.open == closed {
                continue;
            }
            if priority.as_deref().is_some_and(|p| p != t.priority) {
                continue;
            }
            if size.as_deref().is_some_and(|s| s != t.size) {
                continue;
            }
            if unblocked
                && id_tokens(&t.depends)
                    .iter()
                    .any(|b| all.iter().any(|o| o.id == *b && o.open))
            {
                continue;
            }
            out.push_str(&format!(
                "{}  {}  {}  {:<11}  {}\n",
                t.id, t.priority, t.size, t.status, t.title
            ));
        }
        if out.is_empty() {
            out = "no tickets match\n".into();
        }
        Ok(out)
    }

    fn show(&self, args: &[String]) -> Result<String, Error> {
        let (id, flags) = one_id(args)?;
        if let Some(f) = flags.first() {
            return Err(Error::Usage(format!("unexpected argument `{f}`")));
        }
        let t = self.ticket(&id)?;
        let all = self.tickets()?;
        let folder = if t.open { "open" } else { "closed" };
        let blockers = id_tokens(&t.depends);
        let blockers = if blockers.is_empty() {
            "—".to_string()
        } else {
            blockers
                .iter()
                .map(|b| {
                    let state = match all.iter().find(|o| o.id == *b) {
                        Some(o) if o.open => "open",
                        Some(_) => "closed",
                        None => "missing",
                    };
                    format!("{b} ({state})")
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        let text = read(&self.dir.join(folder).join(&t.file))?;
        let boxes: Vec<&str> = text
            .lines()
            .skip_while(|l| !l.starts_with("## Done when"))
            .filter(|l| l.trim_start().starts_with("- ["))
            .collect();
        let mut out = format!(
            "{}  {}\n  {} · {} · {} · blocked by: {}\n  tasks/{}/{}\n",
            t.id, t.title, t.status, t.priority, t.size, blockers, folder, t.file
        );
        if !boxes.is_empty() {
            out.push_str("  done when:\n");
            for b in boxes {
                out.push_str(&format!("    {}\n", b.trim_start().replace("- ", "")));
            }
        }
        Ok(out)
    }

    // ---- ticket files ------------------------------------------------------------------

    fn tickets(&self) -> Result<Vec<Ticket>, Error> {
        let mut out = Vec::new();
        for (folder, open) in [("open", true), ("closed", false)] {
            let dir = self.dir.join(folder);
            let entries = std::fs::read_dir(&dir)
                .map_err(|e| Error::Op(format!("{}: {e}", dir.display())))?;
            for entry in entries.flatten() {
                let path = entry.path();
                let file = entry.file_name().to_string_lossy().to_string();
                if path.extension().and_then(|e| e.to_str()) != Some("md")
                    || id_tokens(&file).first().map(String::as_str) != Some(&file[..5])
                {
                    continue;
                }
                out.push(parse_ticket(&file, open, &read(&path)?));
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    fn ticket(&self, id: &str) -> Result<Ticket, Error> {
        self.tickets()?
            .into_iter()
            .find(|t| t.id == id)
            .ok_or_else(|| Error::Op(format!("no ticket {id} in tasks/open/ or tasks/closed/")))
    }

    fn max_id(&self) -> Result<u32, Error> {
        Ok(self
            .tickets()?
            .iter()
            .filter_map(|t| t.id[2..].parse().ok())
            .max()
            .unwrap_or(0))
    }

    /// `T-020, T-062` with each closed blocker struck through; `—` when empty.
    fn blockers_cell(&self, ids: &[String]) -> String {
        if ids.is_empty() {
            return "—".to_string();
        }
        ids.iter()
            .map(|b| {
                if self.dir.join("closed").join(find_file(&self.dir.join("closed"), b)).is_file() {
                    format!("~~{b}~~")
                } else {
                    b.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    // ---- README ------------------------------------------------------------------------

    fn readme(&self) -> Result<Vec<String>, Error> {
        Ok(read(&self.dir.join("README.md"))?.lines().map(str::to_string).collect())
    }

    fn save_readme(&self, lines: &[String]) -> Result<(), Error> {
        self.write(&self.dir.join("README.md"), &(lines.join("\n") + "\n"))
    }

    /// `(start, end)` of the lines under `heading`, ending at the next same-level heading.
    fn section(lines: &[String], heading: &str) -> Result<(usize, usize), Error> {
        let level = format!("{} ", heading.split(' ').next().unwrap_or("##"));
        let start = lines
            .iter()
            .position(|l| l.starts_with(heading))
            .ok_or_else(|| Error::Op(format!("README has no `{heading}` section")))?;
        let end = lines[start + 1..]
            .iter()
            .position(|l| l.starts_with(&level))
            .map_or(lines.len(), |i| start + 1 + i);
        Ok((start, end))
    }

    /// Remove and return the one table row under `heading` that contains `needle`.
    fn take_row(
        &self,
        lines: &mut Vec<String>,
        heading: &str,
        needle: &str,
    ) -> Result<String, Error> {
        let (start, end) = Self::section(lines, heading)?;
        let at = lines[start..end]
            .iter()
            .position(|l| l.trim_start().starts_with('|') && l.contains(needle))
            .map(|i| start + i)
            .ok_or_else(|| Error::Op(format!("README has no row matching `{needle}`")))?;
        Ok(lines.remove(at))
    }

    /// Append `cells` as a row to the table under `### <priority>` inside `## Open`.
    fn insert_open_row(
        &self,
        lines: &mut Vec<String>,
        priority: &str,
        cells: &[&str],
    ) -> Result<(), Error> {
        let (open_start, open_end) = Self::section(lines, "## Open")?;
        let head = lines[open_start..open_end]
            .iter()
            .position(|l| l.starts_with(&format!("### {priority}")))
            .map(|i| open_start + i)
            .ok_or_else(|| Error::Op(format!("README has no `### {priority}` table")))?;
        let sub_end = lines[head + 1..open_end]
            .iter()
            .position(|l| l.starts_with("### "))
            .map_or(open_end, |i| head + 1 + i);
        self.append_row(lines, head, sub_end, cells)
    }

    fn append_closed_row(&self, lines: &mut Vec<String>, cells: &[&str]) -> Result<(), Error> {
        let (start, end) = Self::section(lines, "## Closed")?;
        self.append_row(lines, start, end, cells)
    }

    /// Append a row after the last `|` line in `lines[start..end]`, padded to the table's
    /// header-row column widths.
    fn append_row(
        &self,
        lines: &mut Vec<String>,
        start: usize,
        end: usize,
        cells: &[&str],
    ) -> Result<(), Error> {
        let rows: Vec<usize> = (start..end)
            .filter(|&i| lines[i].trim_start().starts_with('|'))
            .collect();
        let (&first, &last) = match (rows.first(), rows.last()) {
            (Some(f), Some(l)) => (f, l),
            _ => return Err(Error::Op("README table not found where expected".into())),
        };
        let widths: Vec<usize> =
            lines[first].split('|').map(|c| c.chars().count()).collect();
        let mut row = String::from("|");
        for (i, c) in cells.iter().enumerate() {
            let w = widths.get(i + 1).copied().unwrap_or(0).max(c.chars().count() + 2);
            row.push_str(&format!(" {:<width$} |", c, width = w - 2));
        }
        lines.insert(last + 1, row);
        Ok(())
    }

    /// Strike (`closed == true`) or un-strike `id` in the last cell of every data row
    /// under `## Open` — the Blocked by / Refs column.
    fn mark_blocker(&self, lines: &mut [String], id: &str, closed: bool) {
        let Ok((start, end)) = Self::section(lines, "## Open") else { return };
        for line in &mut lines[start..end] {
            if !line.trim_start().starts_with('|') {
                continue;
            }
            let mut cells: Vec<String> = line.split('|').map(str::to_string).collect();
            let Some(last) = cells.len().checked_sub(2) else { continue };
            cells[last] = if closed {
                strike(&cells[last], id)
            } else {
                cells[last].replace(&format!("~~{id}~~"), id)
            };
            *line = cells.join("|");
        }
    }

    // ---- filesystem, honoring --dry-run ------------------------------------------------

    fn write(&self, path: &Path, content: &str) -> Result<(), Error> {
        if self.dry {
            return Ok(());
        }
        std::fs::write(path, content).map_err(|e| Error::Op(format!("{}: {e}", path.display())))
    }

    fn remove(&self, path: &Path) -> Result<(), Error> {
        if self.dry {
            return Ok(());
        }
        std::fs::remove_file(path).map_err(|e| Error::Op(format!("{}: {e}", path.display())))
    }
}

// ---- pure helpers ----------------------------------------------------------------------

fn read(path: &Path) -> Result<String, Error> {
    std::fs::read_to_string(path).map_err(|e| Error::Op(format!("{}: {e}", path.display())))
}

fn one_id(args: &[String]) -> Result<(String, Vec<String>), Error> {
    let (ids, flags): (Vec<_>, Vec<_>) = args.iter().cloned().partition(|a| !a.starts_with('-'));
    match ids.as_slice() {
        [id] if id_tokens(id).len() == 1 && id.len() == 5 => Ok((id.clone(), flags)),
        [id] => Err(Error::Usage(format!("`{id}` is not a ticket id (T-0NN)"))),
        _ => Err(Error::Usage("expected exactly one ticket id".into())),
    }
}

/// Every `T-` + 3 digits token in `s`, in order.
fn id_tokens(s: &str) -> Vec<String> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 5 <= b.len() {
        if b[i] == b'T'
            && b[i + 1] == b'-'
            && b[i + 2..i + 5].iter().all(u8::is_ascii_digit)
            && b.get(i + 5).is_none_or(|c| !c.is_ascii_digit())
        {
            out.push(s[i..i + 5].to_string());
            i += 5;
        } else {
            i += 1;
        }
    }
    out
}

fn parse_ticket(file: &str, open: bool, text: &str) -> Ticket {
    let title = text
        .lines()
        .next()
        .and_then(|l| l.split_once(" — "))
        .map(|(_, t)| t.to_string())
        .unwrap_or_default();
    let row: Vec<String> = text
        .lines()
        .filter(|l| l.trim_start().starts_with('|'))
        .nth(2)
        .map(|l| l.split('|').map(|c| c.trim().trim_matches('*').to_string()).collect())
        .unwrap_or_default();
    let get = |i: usize| row.get(i).cloned().unwrap_or_default();
    Ticket {
        id: file[..5].to_string(),
        file: file.to_string(),
        open,
        title,
        status: get(1),
        priority: get(2),
        size: get(3),
        depends: get(4),
    }
}

/// Nth `|`-separated cell of a table row, trimmed. 1-based like split gives it.
fn cell(row: &str, n: usize) -> String {
    row.split('|').nth(n).map(str::trim).unwrap_or_default().to_string()
}

fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= 60 {
            break;
        }
    }
    out.trim_matches('-').to_string()
}

/// A 3-line markdown table, columns padded to the widest of header and value.
fn header_table(headers: &[&str], values: &[&str]) -> String {
    let widths: Vec<usize> = headers
        .iter()
        .zip(values)
        .map(|(h, v)| h.chars().count().max(v.chars().count()))
        .collect();
    let line = |cells: &[&str]| {
        let mut s = String::from("|");
        for (c, w) in cells.iter().zip(&widths) {
            s.push_str(&format!(" {:<width$} |", c, width = w));
        }
        s
    };
    let sep: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
    let sep: Vec<&str> = sep.iter().map(String::as_str).collect();
    format!("{}\n{}\n{}", line(headers), line(&sep), line(values))
}

/// Rewrite the status cell of the ticket's header table, re-padding all three lines.
fn set_status(text: &str, status: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let idx: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.trim_start().starts_with('|'))
        .map(|(i, _)| i)
        .take(3)
        .collect();
    if let [h, s, d] = idx.as_slice() {
        let headers: Vec<String> = lines[*h].split('|').map(|c| c.trim().to_string()).collect();
        let mut values: Vec<String> = lines[*d].split('|').map(|c| c.trim().to_string()).collect();
        if values.len() > 1 {
            values[1] = status.to_string();
            let headers: Vec<&str> = headers[1..headers.len() - 1].iter().map(String::as_str).collect();
            let values: Vec<&str> = values[1..values.len() - 1].iter().map(String::as_str).collect();
            let table = header_table(&headers, &values);
            let mut t = table.lines();
            for i in [h, s, d] {
                lines[*i] = t.next().unwrap_or_default().to_string();
            }
        }
    }
    lines.join("\n") + "\n"
}

/// `T-020` -> `~~T-020~~` where it appears as a token, unless already struck.
fn strike(cell: &str, id: &str) -> String {
    let mut out = String::new();
    let mut rest = cell;
    while let Some(pos) = rest.find(id) {
        let after = &rest[pos + id.len()..];
        out.push_str(&rest[..pos]);
        let already = out.ends_with('~') || after.starts_with('~');
        let longer_id = after.starts_with(|c: char| c.is_ascii_digit());
        if already || longer_id {
            out.push_str(id);
        } else {
            out.push_str(&format!("~~{id}~~"));
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

fn find_file(dir: &Path, id: &str) -> String {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .find(|f| f.starts_with(&format!("{id}-")))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("board-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("open")).unwrap();
        std::fs::create_dir_all(dir.join("closed")).unwrap();
        let ticket = |id: &str, status: &str, prio: &str, size: &str, deps: &str, title: &str| {
            format!(
                "# {id} — {title}\n\n{}\n\n## Problem\n\ntext\n\n## Done when\n\n- [ ] a thing\n",
                header_table(&["Status", "Priority", "Size", "Depends on"], &[
                    status, prio, size, deps,
                ])
            )
        };
        std::fs::write(
            dir.join("open/T-001-a.md"),
            ticket("T-001", "open", "P1", "S", "T-002", "Alpha"),
        )
        .unwrap();
        std::fs::write(
            dir.join("open/T-002-b.md"),
            ticket("T-002", "open", "P2", "M", "—", "Beta"),
        )
        .unwrap();
        std::fs::write(
            dir.join("closed/T-003-c.md"),
            ticket("T-003", "done", "P1", "S", "—", "Gamma"),
        )
        .unwrap();
        std::fs::write(
            dir.join("README.md"),
            "\
# Board

intro prose

## Open

### P1 — bad

| ID    | Title                | Size | Blocked by |
| ----- | -------------------- | ---- | ---------- |
| T-001 | [Alpha](open/T-001-a.md) | S | T-002    |

### P2 — meh

| ID    | Title                | Size | Refs |
| ----- | -------------------- | ---- | ---- |
| T-002 | [Beta](open/T-002-b.md) | M | —    |

## Closed

| ID    | Title                | Outcome |
| ----- | -------------------- | ------- |
| T-003 | [Gamma](closed/T-003-c.md) | done |

## Settled — don't re-derive these

prose that must survive
",
        )
        .unwrap();
        dir
    }

    fn go(dir: &Path, args: &[&str]) -> Result<String, Error> {
        let mut v: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        v.extend(["--dir".to_string(), dir.to_string_lossy().to_string()]);
        run(&v)
    }

    #[test]
    fn close_does_all_three_edits_and_strikes_blockers() {
        let d = fixture("close");
        go(&d, &["close", "T-002"]).unwrap();
        assert!(!d.join("open/T-002-b.md").exists());
        let moved = std::fs::read_to_string(d.join("closed/T-002-b.md")).unwrap();
        assert!(moved.contains("| done "), "status flipped: {moved}");
        let readme = std::fs::read_to_string(d.join("README.md")).unwrap();
        assert!(readme.contains("[Beta](closed/T-002-b.md)"));
        assert!(!readme.contains("[Beta](open/"));
        assert!(readme.contains("~~T-002~~"), "T-001's blocker struck: {readme}");
        assert!(readme.contains("prose that must survive"));
        assert!(readme.contains("intro prose"));
    }

    #[test]
    fn reopen_reverses_close() {
        let d = fixture("reopen");
        go(&d, &["close", "T-002"]).unwrap();
        go(&d, &["reopen", "T-002"]).unwrap();
        assert!(d.join("open/T-002-b.md").exists());
        let back = std::fs::read_to_string(d.join("open/T-002-b.md")).unwrap();
        assert!(back.contains("| open "), "status back to open: {back}");
        let readme = std::fs::read_to_string(d.join("README.md")).unwrap();
        assert!(readme.contains("[Beta](open/T-002-b.md)"));
        assert!(!readme.contains("~~T-002~~"), "blocker un-struck: {readme}");
        let p2 = readme.split("### P2").nth(1).unwrap();
        assert!(p2.contains("T-002"), "row back under its priority: {p2}");
    }

    #[test]
    fn new_scaffolds_file_and_row() {
        let d = fixture("new");
        let out = go(&d, &[
            "new", "Hover shows wrong path", "-p", "P1", "-s", "S", "-b", "T-003",
            "--problem", "It lies.",
        ])
        .unwrap();
        assert!(out.contains("T-004-hover-shows-wrong-path.md"), "{out}");
        let body = std::fs::read_to_string(d.join("open/T-004-hover-shows-wrong-path.md")).unwrap();
        assert!(body.starts_with("# T-004 — Hover shows wrong path"));
        assert!(body.contains("| open   | P1"));
        assert!(body.contains("## Problem\n\nIt lies."));
        assert!(body.contains("## Done when"));
        let readme = std::fs::read_to_string(d.join("README.md")).unwrap();
        let p1 = readme.split("### P1").nth(1).unwrap().split("### P2").next().unwrap();
        assert!(p1.contains("[Hover shows wrong path](open/T-004-hover-shows-wrong-path.md)"));
        assert!(p1.contains("~~T-003~~"), "closed blocker arrives struck: {p1}");
    }

    #[test]
    fn dry_run_writes_nothing() {
        let d = fixture("dry");
        let before = std::fs::read_to_string(d.join("README.md")).unwrap();
        go(&d, &["close", "T-002", "--dry-run"]).unwrap();
        assert!(d.join("open/T-002-b.md").exists());
        assert_eq!(before, std::fs::read_to_string(d.join("README.md")).unwrap());
    }

    #[test]
    fn errors_are_operational_not_panics() {
        let d = fixture("err");
        assert!(matches!(go(&d, &["close", "T-999"]), Err(Error::Op(_))));
        assert!(matches!(go(&d, &["close", "T-003"]), Err(Error::Op(_)))); // already closed
        assert!(matches!(go(&d, &["reopen", "T-001"]), Err(Error::Op(_)))); // already open
        assert!(matches!(go(&d, &["close", "nope"]), Err(Error::Usage(_))));
    }

    #[test]
    fn list_filters_and_unblocked() {
        let d = fixture("list");
        let all = go(&d, &["list"]).unwrap();
        assert!(all.contains("T-001") && all.contains("T-002"));
        let unblocked = go(&d, &["list", "--unblocked"]).unwrap();
        assert!(!unblocked.contains("T-001"), "T-001 blocked by open T-002: {unblocked}");
        assert!(unblocked.contains("T-002"));
        go(&d, &["close", "T-002"]).unwrap();
        let after = go(&d, &["list", "--unblocked"]).unwrap();
        assert!(after.contains("T-001"), "unblocked once T-002 closed: {after}");
    }

    #[test]
    fn sync_rederives_the_row_after_hand_edits() {
        let d = fixture("sync");
        let path = d.join("open/T-002-b.md");
        let edited = std::fs::read_to_string(&path)
            .unwrap()
            .replace("| P2       | M    | —", "| P1       | L    | T-003");
        std::fs::write(&path, edited).unwrap();
        go(&d, &["sync", "T-002"]).unwrap();
        let readme = std::fs::read_to_string(d.join("README.md")).unwrap();
        let p1 = readme.split("### P1").nth(1).unwrap().split("### P2").next().unwrap();
        assert!(p1.contains("[Beta](open/T-002-b.md)"), "row moved to P1, title kept: {p1}");
        assert!(p1.contains("| L"), "size updated: {p1}");
        assert!(p1.contains("~~T-003~~"), "closed blocker struck: {p1}");
        let p2 = readme.split("### P2").nth(1).unwrap().split("## Closed").next().unwrap();
        assert!(!p2.contains("T-002-b.md"), "old row gone: {p2}");
    }

    #[test]
    fn show_prints_blocker_state_and_boxes() {
        let d = fixture("show");
        let out = go(&d, &["show", "T-001"]).unwrap();
        assert!(out.contains("T-002 (open)"), "{out}");
        assert!(out.contains("[ ] a thing"), "{out}");
    }
}
