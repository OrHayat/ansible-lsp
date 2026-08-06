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
      -k, --kind <task|bug|epic>  default task; picks the body template
      -e, --epic     <T-0NN>      file under an epic, ticking into its
                                  ## Children list
      -b, --blocked-by <IDS>      comma-separated: T-020,T-062
      --problem <TEXT>            pre-fill the first prose section
  close <T-0NN>                   move to tasks/closed/, outcome done
      --rejected                  outcome rejected instead
  reopen <T-0NN>                  move back to tasks/open/
  sync <T-0NN>                    re-derive the README row from the ticket's
                                  header table (after editing priority/size/
                                  kind/blocked-by in the file)
  list                            open tickets, one line each
      -p, --priority <P1|P2|P3>   filter
      -s, --size     <S|M|L>      filter
      -k, --kind <task|bug|epic>  filter
      -e, --epic     <T-0NN>      only that epic's children
      --no-epic                   only tickets under no epic (epics
                                  themselves are never listed)
      --unblocked                 only tickets with no open blockers
      --closed                    list closed tickets instead
  adopt <T-0NN> -e <T-0NN>        put an existing ticket under an epic —
                                  writes the child's Epic column and the
                                  epic's ## Children line together
  orphan <T-0NN>                  take it back out, both halves
  show <T-0NN>                    one ticket in full; an epic also lists its
                                  children and their state
  upstream                        index upstream/*.md dossiers — these are
                                  bugs in ansible/ansible, not our tickets,
                                  so they live as prose, not in tasks/
      <NAME>                      show one dossier's issues
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
        Some("adopt") => board.adopt(&rest[1..]),
        Some("orphan") => board.orphan(&rest[1..]),
        Some("upstream") => board.upstream(&rest[1..]),
        Some("help") | Some("-h") | Some("--help") => Ok(format!("{USAGE}\n")),
        Some(c) => Err(Error::Usage(format!("unknown command `{c}`"))),
        None => Err(Error::Usage("no command given".into())),
    }
}

struct Board {
    dir: PathBuf,
    dry: bool,
}

/// One ticket file's header table plus its identity on disk. Columns are read by header
/// name, not position, so the 88 tickets written before `Kind`/`Epic` existed still parse —
/// they just come back as plain tasks with no epic.
struct Ticket {
    id: String,
    file: String,
    open: bool,
    title: String,
    status: String,
    kind: String,
    priority: String,
    size: String,
    epic: String,
    depends: String,
}

/// What a ticket is, which decides its body template and its README badge. `upstream` is
/// deliberately absent: those are bugs in ansible/ansible, indexed from `upstream/` by the
/// `upstream` command, so a finding is never both a dossier and a ticket.
const KINDS: [&str; 3] = ["task", "bug", "epic"];

impl Board {
    // ---- commands ----------------------------------------------------------------------

    fn new_ticket(&self, args: &[String]) -> Result<String, Error> {
        let mut title = None;
        let mut priority = None;
        let mut size = None;
        let mut kind = None;
        let mut epic = None;
        let mut blocked: Vec<String> = Vec::new();
        let mut problem = None;
        let mut it = args.iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "-p" | "--priority" => priority = it.next().cloned(),
                "-s" | "--size" => size = it.next().cloned(),
                "-k" | "--kind" => kind = it.next().cloned(),
                "-e" | "--epic" => epic = it.next().cloned(),
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
        let kind = kind.unwrap_or_else(|| "task".into());
        if !["P1", "P2", "P3"].contains(&priority.as_str()) {
            return Err(Error::Usage(format!("priority must be P1|P2|P3, got `{priority}`")));
        }
        if !["S", "M", "L"].contains(&size.as_str()) {
            return Err(Error::Usage(format!("size must be S|M|L, got `{size}`")));
        }
        if !KINDS.contains(&kind.as_str()) {
            return Err(Error::Usage(format!(
                "kind must be {}, got `{kind}` \
                 (upstream findings are dossiers in upstream/, not tickets — see `board upstream`)",
                KINDS.join("|")
            )));
        }
        for b in &blocked {
            if id_tokens(b).len() != 1 {
                return Err(Error::Usage(format!("`{b}` is not a ticket id (T-0NN)")));
            }
        }
        // Resolve the parent before writing anything, so a typo'd epic doesn't leave a
        // half-filed child behind.
        let parent = match &epic {
            Some(e) => {
                let p = self.ticket(e)?;
                if p.kind != "epic" {
                    return Err(Error::Op(format!("{e} is a {}, not an epic", p.kind)));
                }
                Some(p)
            }
            None => None,
        };

        let id = format!("T-{:03}", self.max_id()? + 1);
        let file = format!("{id}-{}.md", slug(&title));
        let deps = if blocked.is_empty() { "—".to_string() } else { blocked.join(", ") };
        let (mut headers, mut values) = (
            vec!["Status", "Kind", "Priority", "Size"],
            vec!["open", kind.as_str(), priority.as_str(), size.as_str()],
        );
        if let Some(e) = &epic {
            headers.push("Epic");
            values.push(e);
        }
        headers.push("Depends on");
        values.push(&deps);

        let mut body =
            format!("# {id} — {title}\n\n{}\n", header_table(&headers, &values));
        for (n, section) in sections_for(&kind).iter().enumerate() {
            body.push_str(&format!("\n## {section}\n"));
            match problem.as_deref().filter(|_| n == 0) {
                Some(p) => body.push_str(&format!("\n{p}\n")),
                None => {}
            }
        }
        body.push_str("\n## Done when\n\n- [ ]\n");

        self.write(&self.dir.join("open").join(&file), &body)?;

        let mut lines = self.readme()?;
        let cell = self.blockers_cell(&blocked);
        let row = [&id, &format!("{}[{title}](open/{file})", badge(&kind)), &size, &cell];
        self.insert_open_row(&mut lines, &priority, &row.map(String::as_str))?;
        self.save_readme(&lines)?;

        // The epic's checklist is the parent->child half of the link; the child's own
        // `Epic` column is the other. Both are written here so neither can be forgotten.
        if let Some(p) = &parent {
            self.add_child(p, &id, &title, false)?;
        }

        Ok(format!(
            "created tasks/open/{file}  ({kind})\nREADME: added row under ### {priority}\n{}{}",
            match &epic {
                Some(e) => format!("{e}: listed under ## Children\n"),
                None => String::new(),
            },
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
        // A closed epic with open children is exactly the drift board.rs exists to catch,
        // so refuse it here rather than let the test find it later.
        if t.kind == "epic" && !rejected {
            let open: Vec<String> = self
                .children(&id)?
                .into_iter()
                .filter(|c| c.open)
                .map(|c| format!("{} {}", c.id, c.title))
                .collect();
            if !open.is_empty() {
                return Err(Error::Op(format!(
                    "{id} is an epic with {} open {}:\n  {}\nclose them first, or `close {id} --rejected` to drop the epic",
                    open.len(),
                    if open.len() == 1 { "child" } else { "children" },
                    open.join("\n  "),
                )));
            }
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
        self.set_child_box(&t, true)?;

        Ok(format!(
            "moved tasks/open/{f} -> tasks/closed/\nstatus {} -> {}\nREADME: row moved to ## Closed\n{}{}",
            t.status,
            outcome.trim_matches('*'),
            if t.epic.is_empty() || t.epic == "—" {
                String::new()
            } else {
                format!("{}: ticked {id} in ## Children\n", t.epic)
            },
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
        self.set_child_box(&t, false)?;

        Ok(format!(
            "moved tasks/closed/{f} -> tasks/open/\nstatus {} -> open\nREADME: row moved to ### {}\n{}{}",
            t.status,
            t.priority,
            if t.epic.is_empty() || t.epic == "—" {
                String::new()
            } else {
                format!("{}: unticked {id} in ## Children\n", t.epic)
            },
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
        let link = format!("{}[{title}]({folder}/{})", badge(&t.kind), t.file);

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
        let mut kind = None;
        let mut epic = None;
        let mut no_epic = false;
        let mut unblocked = false;
        let mut closed = false;
        let mut it = args.iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "-p" | "--priority" => priority = it.next().cloned(),
                "-s" | "--size" => size = it.next().cloned(),
                "-k" | "--kind" => kind = it.next().cloned(),
                "-e" | "--epic" => epic = it.next().cloned(),
                "--no-epic" => no_epic = true,
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
            if kind.as_deref().is_some_and(|k| k != t.kind) {
                continue;
            }
            if epic.as_deref().is_some_and(|e| e != t.epic) {
                continue;
            }
            // An epic is not its own orphan — it has no parent by definition.
            if no_epic && (t.kind == "epic" || !(t.epic.is_empty() || t.epic == "—")) {
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
                "{}  {:<4}  {}  {}  {:<11}  {}\n",
                t.id, t.kind, t.priority, t.size, t.status, t.title
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
            "{}  {}\n  {} · {} · {} · {} · blocked by: {}\n  tasks/{}/{}\n",
            t.id, t.title, t.kind, t.status, t.priority, t.size, blockers, folder, t.file
        );
        if !t.epic.is_empty() && t.epic != "—" {
            let parent = all.iter().find(|o| o.id == t.epic);
            out.push_str(&format!(
                "  epic: {} {}\n",
                t.epic,
                parent.map_or("(missing)".into(), |p| p.title.clone())
            ));
        }
        // State comes from the folders, never from the checkbox — the checklist is a
        // convenience, `tasks/{open,closed}/` is the truth.
        let kids = self.children(&t.id)?;
        if !kids.is_empty() {
            let done = kids.iter().filter(|c| !c.open).count();
            out.push_str(&format!("  children ({done}/{} done):\n", kids.len()));
            for c in &kids {
                out.push_str(&format!(
                    "    [{}] {}  {}  {}\n",
                    if c.open { ' ' } else { 'x' },
                    c.id,
                    c.priority,
                    c.title
                ));
            }
        }
        if !boxes.is_empty() {
            out.push_str("  done when:\n");
            for b in boxes {
                out.push_str(&format!("    {}\n", b.trim_start().replace("- ", "")));
            }
        }
        Ok(out)
    }

    /// Dossiers in `upstream/` — bugs in ansible/ansible, which are prose, not tickets.
    /// Nothing is migrated to make this work: the shape below is read off the files as
    /// they're already written.
    fn upstream(&self, args: &[String]) -> Result<String, Error> {
        let dir = self.dir.join("../upstream");
        let mut files: Vec<(String, String)> = std::fs::read_dir(&dir)
            .map_err(|e| Error::Op(format!("{}: {e}", dir.display())))?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
            .filter_map(|p| {
                let name = p.file_stem()?.to_string_lossy().to_string();
                Some((name, std::fs::read_to_string(&p).ok()?))
            })
            .collect();
        files.sort_by(|a, b| a.0.cmp(&b.0));

        if let Some(want) = args.iter().find(|a| !a.starts_with('-')) {
            let (name, text) = files
                .into_iter()
                .find(|(n, _)| n == want || n.ends_with(want.as_str()))
                .ok_or_else(|| Error::Op(format!("no dossier `{want}` in upstream/")))?;
            let d = Dossier::parse(&name, &text);
            let mut out = format!("{}  {}\n  upstream/{name}.md\n", d.name, d.title);
            for (n, issue) in d.issues.iter().enumerate() {
                out.push_str(&format!("  {}. {issue}\n", n + 1));
            }
            if !d.refs.is_empty() {
                out.push_str(&format!("  filed: {}\n", d.refs.join(", ")));
            }
            return Ok(out);
        }

        let mut out = String::new();
        for (name, text) in &files {
            let d = Dossier::parse(name, text);
            // A single-topic dossier has no `## Issue N` headings but is still one issue.
            let n = d.issues.len().max(1);
            out.push_str(&format!(
                "{:<24}  {:<22}  {n} issue{}  {}\n",
                d.name,
                if d.refs.is_empty() { "not filed".into() } else { d.refs.join(",") },
                if n == 1 { "" } else { "s" },
                d.title,
            ));
        }
        if out.is_empty() {
            out = "no dossiers in upstream/\n".into();
        }
        Ok(out)
    }

    // ---- epic <-> child ----------------------------------------------------------------

    /// Every ticket whose `Epic` column names `id`, in id order.
    fn children(&self, id: &str) -> Result<Vec<Ticket>, Error> {
        Ok(self.tickets()?.into_iter().filter(|t| t.epic == id).collect())
    }

    /// Append `- [ ] T-0NN — Title` to the epic's `## Children` list, creating the section
    /// just above `## Done when` if the epic hasn't got one yet. `done` ticks the box on
    /// arrival, which is how an already-closed ticket gets adopted without lying.
    fn add_child(
        &self,
        parent: &Ticket,
        id: &str,
        title: &str,
        done: bool,
    ) -> Result<(), Error> {
        let folder = if parent.open { "open" } else { "closed" };
        let path = self.dir.join(folder).join(&parent.file);
        let mut lines: Vec<String> = read(&path)?.lines().map(str::to_string).collect();
        let entry = format!("- [{}] {id} — {title}", if done { 'x' } else { ' ' });

        match lines.iter().position(|l| l.starts_with("## Children")) {
            Some(head) => {
                let end = lines[head + 1..]
                    .iter()
                    .position(|l| l.starts_with("## "))
                    .map_or(lines.len(), |i| head + 1 + i);
                let at = (head + 1..end)
                    .filter(|&i| lines[i].starts_with("- ["))
                    .next_back()
                    .map_or(end, |i| i + 1);
                lines.insert(at, entry);
                // The first child of an empty section lands directly on the next heading's
                // line, which would leave the list glued to it.
                if lines.get(at + 1).is_some_and(|l| l.starts_with("## ")) {
                    lines.insert(at + 1, String::new());
                }
            }
            None => {
                let at = lines
                    .iter()
                    .position(|l| l.starts_with("## Done when"))
                    .unwrap_or(lines.len());
                for (n, l) in
                    ["## Children".to_string(), String::new(), entry, String::new()]
                        .into_iter()
                        .enumerate()
                {
                    lines.insert(at + n, l);
                }
            }
        }
        self.write(&path, &(lines.join("\n") + "\n"))
    }

    /// Drop `id`'s line from the epic's `## Children` list.
    fn remove_child(&self, parent: &Ticket, id: &str) -> Result<(), Error> {
        let folder = if parent.open { "open" } else { "closed" };
        let path = self.dir.join(folder).join(&parent.file);
        let mut lines: Vec<String> = read(&path)?.lines().map(str::to_string).collect();
        lines.retain(|l| !(l.starts_with("- [") && l.get(6..11) == Some(id)));
        self.write(&path, &(lines.join("\n") + "\n"))
    }

    /// Put an existing ticket under an epic — the half `new -e` can't do, because by the
    /// time you know a ticket belongs in a group it's usually already filed.
    fn adopt(&self, args: &[String]) -> Result<String, Error> {
        let (id, epic) = two_ids(args, "adopt")?;
        let epic = epic.ok_or_else(|| Error::Usage("adopt needs --epic <T-0NN>".into()))?;
        if id == epic {
            return Err(Error::Op(format!("{id} cannot be its own epic")));
        }
        let t = self.ticket(&id)?;
        let parent = self.ticket(&epic)?;
        if parent.kind != "epic" {
            return Err(Error::Op(format!("{epic} is a {}, not an epic", parent.kind)));
        }
        if t.kind == "epic" {
            return Err(Error::Op(format!(
                "{id} is itself an epic — epics don't nest, split the work instead"
            )));
        }
        match t.epic.as_str() {
            e if e == epic => return Err(Error::Op(format!("{id} is already under {epic}"))),
            "" | "—" => {}
            other => {
                return Err(Error::Op(format!(
                    "{id} already belongs to {other} — `board orphan {id}` first if you mean to move it"
                )))
            }
        }

        let folder = if t.open { "open" } else { "closed" };
        let path = self.dir.join(folder).join(&t.file);
        let text = read(&path)?;
        self.write(&path, &set_header_field(&text, "Epic", Some(&epic)))?;
        // A closed ticket arrives already ticked, so the checklist never claims work is
        // outstanding that shipped months ago.
        self.add_child(&parent, &id, &t.title, !t.open)?;

        Ok(format!(
            "{id}: Epic -> {epic}\n{epic}: listed {id} in ## Children{}\n{}",
            if t.open { "" } else { " (ticked — already closed)" },
            if self.dry { "(dry run — nothing written)\n" } else { "" }
        ))
    }

    /// Pull a ticket back out of its epic. Both halves again, so neither can be left behind.
    fn orphan(&self, args: &[String]) -> Result<String, Error> {
        let (id, _) = two_ids(args, "orphan")?;
        let t = self.ticket(&id)?;
        if t.epic.is_empty() || t.epic == "—" {
            return Err(Error::Op(format!("{id} is not under an epic")));
        }
        let epic = t.epic.clone();

        let folder = if t.open { "open" } else { "closed" };
        let path = self.dir.join(folder).join(&t.file);
        let text = read(&path)?;
        self.write(&path, &set_header_field(&text, "Epic", None))?;
        if let Ok(parent) = self.ticket(&epic) {
            self.remove_child(&parent, &id)?;
        }

        Ok(format!(
            "{id}: Epic column removed\n{epic}: dropped {id} from ## Children\n{}",
            if self.dry { "(dry run — nothing written)\n" } else { "" }
        ))
    }

    /// Tick or untick `t`'s line in its epic's checklist. A no-op when the ticket has no
    /// epic, or the epic has no line for it.
    fn set_child_box(&self, t: &Ticket, done: bool) -> Result<(), Error> {
        if t.epic.is_empty() || t.epic == "—" {
            return Ok(());
        }
        let Ok(parent) = self.ticket(&t.epic) else { return Ok(()) };
        let folder = if parent.open { "open" } else { "closed" };
        let path = self.dir.join(folder).join(&parent.file);
        let text = read(&path)?;
        let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
        let (from, to) = if done { ("- [ ] ", "- [x] ") } else { ("- [x] ", "- [ ] ") };
        for line in &mut lines {
            if line.starts_with(from) && line[6..].starts_with(&t.id) {
                *line = format!("{to}{}", &line[6..]);
            }
        }
        self.write(&path, &(lines.join("\n") + "\n"))
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

/// `<T-0NN> [-e|--epic <T-0NN>]` — the shape `adopt` and `orphan` share.
fn two_ids(args: &[String], cmd: &str) -> Result<(String, Option<String>), Error> {
    let (mut id, mut epic) = (None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-e" | "--epic" => epic = it.next().cloned(),
            _ if a.starts_with('-') => {
                return Err(Error::Usage(format!("unexpected argument `{a}`")))
            }
            _ if id.is_none() => id = Some(a.clone()),
            _ => return Err(Error::Usage(format!("{cmd} takes one ticket id, got `{a}` too"))),
        }
    }
    let id = id.ok_or_else(|| Error::Usage(format!("{cmd} needs a ticket id")))?;
    for v in [Some(&id), epic.as_ref()].into_iter().flatten() {
        if id_tokens(v).len() != 1 || v.len() != 5 {
            return Err(Error::Usage(format!("`{v}` is not a ticket id (T-0NN)")));
        }
    }
    Ok((id, epic))
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
    let get = |name: &str| header_cell(text, name);
    Ticket {
        id: file[..5].to_string(),
        file: file.to_string(),
        open,
        title,
        status: get("Status"),
        // Written before Kind existed == a plain task.
        kind: Some(get("Kind")).filter(|k| !k.is_empty()).unwrap_or_else(|| "task".into()),
        priority: get("Priority"),
        size: get("Size"),
        epic: get("Epic"),
        depends: get("Depends on"),
    }
}

/// The value under `name` in a ticket's 3-line header table, or `""` if there's no such
/// column. Position-independent so `Kind` and `Epic` could be added without touching the
/// tickets that predate them.
fn header_cell(text: &str, name: &str) -> String {
    let rows: Vec<&str> =
        text.lines().filter(|l| l.trim_start().starts_with('|')).take(3).collect();
    let [head, _, values] = rows.as_slice() else { return String::new() };
    let cells = |l: &str| -> Vec<String> {
        l.split('|').map(|c| c.trim().trim_matches('*').to_string()).collect()
    };
    let head = cells(head);
    let values = cells(values);
    head.iter()
        .position(|h| h == name)
        .and_then(|i| values.get(i).cloned())
        .unwrap_or_default()
}

/// Nth `|`-separated cell of a table row, trimmed. 1-based like split gives it.
fn cell(row: &str, n: usize) -> String {
    row.split('|').nth(n).map(str::trim).unwrap_or_default().to_string()
}

/// The prose headings a new ticket of this kind starts with, before `## Done when`. A bug
/// asks what's wrong before what to do; an epic carries the child list instead of an
/// approach, because the approach lives in the children.
fn sections_for(kind: &str) -> &'static [&'static str] {
    match kind {
        "bug" => &["Symptom", "Cause", "Fix"],
        "epic" => &["Problem", "Children"],
        _ => &["Problem", "Approach"],
    }
}

/// The README title-cell prefix for a kind. Tasks get nothing, so the 88 rows that predate
/// kinds stay byte-identical.
fn badge(kind: &str) -> String {
    match kind {
        "task" | "" => String::new(),
        k => format!("**{k}** · "),
    }
}

/// An `upstream/*.md` dossier as the CLI sees it. Derived from the prose — these files are
/// written for humans first and are not migrated to a header table.
struct Dossier {
    name: String,
    title: String,
    issues: Vec<String>,
    refs: Vec<String>,
}

impl Dossier {
    fn parse(name: &str, text: &str) -> Self {
        let title = text
            .lines()
            .find_map(|l| l.strip_prefix("# "))
            .map(|t| t.split_once(" — ").map_or(t.to_string(), |(_, r)| r.to_string()))
            .unwrap_or_default();
        // `## Issue 1 — ...` in a multi-issue dossier; single-topic files have none and
        // count as one issue, which is why the caller uses `.max(1)`.
        let issues: Vec<String> = text
            .lines()
            .filter_map(|l| l.strip_prefix("## Issue "))
            .map(|l| l.split_once(" — ").map_or(l.to_string(), |(_, r)| r.to_string()))
            .collect();
        let mut refs: Vec<String> = Vec::new();
        for (marker, prefix) in [("/issues/", "#"), ("/pull/", "PR#")] {
            for (_, rest) in text.match_indices("github.com/ansible/ansible").map(|(i, _)| {
                (i, &text[i..])
            }) {
                let Some(num) = rest.strip_prefix("github.com/ansible/ansible").and_then(|r| {
                    r.strip_prefix(marker)
                }) else {
                    continue;
                };
                let n: String = num.chars().take_while(char::is_ascii_digit).collect();
                let entry = format!("{prefix}{n}");
                if !n.is_empty() && !refs.contains(&entry) {
                    refs.push(entry);
                }
            }
        }
        Self { name: name.to_string(), title, issues, refs }
    }
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
    set_header_field(text, "Status", Some(status))
}

/// Add, update, or — with `None` — remove a column in the ticket's 3-line header table,
/// re-padding all three lines. A column that doesn't exist yet is inserted before
/// `Depends on`, which is the order `new` writes, so an adopted ticket's header ends up
/// identical to one born under its epic.
fn set_header_field(text: &str, name: &str, value: Option<&str>) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let idx: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.trim_start().starts_with('|'))
        .map(|(i, _)| i)
        .take(3)
        .collect();
    let [h, s, d] = idx.as_slice() else { return text.to_string() };
    // Trim whitespace but not emphasis: `**rejected**` has to survive a rewrite.
    let split = |l: &str| -> Vec<String> {
        let mut cells: Vec<String> = l.split('|').map(|c| c.trim().to_string()).collect();
        cells.pop();
        if !cells.is_empty() {
            cells.remove(0);
        }
        cells
    };
    let mut headers = split(&lines[*h]);
    let mut values = split(&lines[*d]);
    if headers.is_empty() {
        return text.to_string();
    }
    values.resize(headers.len(), String::new());

    match (headers.iter().position(|x| x == name), value) {
        (Some(i), Some(v)) => values[i] = v.to_string(),
        (Some(i), None) => {
            headers.remove(i);
            values.remove(i);
        }
        (None, Some(v)) => {
            let at = headers.iter().position(|x| x == "Depends on").unwrap_or(headers.len());
            headers.insert(at, name.to_string());
            values.insert(at, v.to_string());
        }
        (None, None) => return text.to_string(),
    }

    let hs: Vec<&str> = headers.iter().map(String::as_str).collect();
    let vs: Vec<&str> = values.iter().map(String::as_str).collect();
    let table = header_table(&hs, &vs);
    let mut t = table.lines();
    for i in [h, s, d] {
        lines[*i] = t.next().unwrap_or_default().to_string();
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
        assert!(body.contains("| open   | task | P1"), "kind defaults to task: {body}");
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

    /// The 88 tickets written before Kind existed have no such column; they must keep
    /// parsing, and come back as plain tasks.
    #[test]
    fn tickets_predating_kind_read_as_tasks() {
        let d = fixture("legacy");
        let out = go(&d, &["show", "T-001"]).unwrap();
        assert!(out.contains("task"), "no Kind column -> task: {out}");
        let listed = go(&d, &["list", "-k", "task"]).unwrap();
        assert!(listed.contains("T-001") && listed.contains("T-002"));
        // And a positional read would have taken P1 for the status.
        assert!(out.contains("open · P1 · S"), "columns still land right: {out}");
    }

    #[test]
    fn new_writes_the_kind_and_its_template() {
        let d = fixture("kind");
        go(&d, &["new", "Hover lies on Windows", "-p", "P1", "-s", "S", "-k", "bug"]).unwrap();
        let body = std::fs::read_to_string(d.join("open/T-004-hover-lies-on-windows.md")).unwrap();
        assert!(body.contains("| Kind"), "header has a Kind column: {body}");
        assert!(body.contains("| bug"), "{body}");
        assert!(body.contains("## Symptom") && body.contains("## Cause"), "bug template: {body}");
        assert!(!body.contains("## Approach"), "bug gets Fix, not Approach: {body}");
        let readme = std::fs::read_to_string(d.join("README.md")).unwrap();
        assert!(readme.contains("**bug** · [Hover lies on Windows](open/"), "badged: {readme}");
        // A task stays unbadged so the rows that predate kinds are untouched.
        go(&d, &["new", "Plain thing", "-p", "P2", "-s", "M"]).unwrap();
        let readme = std::fs::read_to_string(d.join("README.md")).unwrap();
        assert!(readme.contains("| [Plain thing](open/"), "task unbadged: {readme}");
    }

    #[test]
    fn epic_links_both_ways_and_tracks_children() {
        let d = fixture("epic");
        go(&d, &["new", "Resolver diverges", "-p", "P1", "-s", "L", "-k", "epic"]).unwrap();
        go(&d, &["new", "Wrong extension order", "-p", "P1", "-s", "S", "-k", "bug", "-e", "T-004"])
            .unwrap();

        let child = std::fs::read_to_string(d.join("open/T-005-wrong-extension-order.md")).unwrap();
        assert!(child.contains("| Epic") && child.contains("| T-004"), "child names it: {child}");
        let epic = std::fs::read_to_string(d.join("open/T-004-resolver-diverges.md")).unwrap();
        assert!(
            epic.contains("- [ ] T-005 — Wrong extension order"),
            "epic lists it: {epic}"
        );
        assert!(
            !epic.contains("Wrong extension order\n## "),
            "a blank line separates the list from the next heading: {epic}"
        );

        let shown = go(&d, &["show", "T-004"]).unwrap();
        assert!(shown.contains("children (0/1 done)"), "{shown}");
        assert!(go(&d, &["list", "-e", "T-004"]).unwrap().contains("T-005"));

        // The link is not a blocker: a child is workable the moment it is filed.
        assert!(go(&d, &["list", "--unblocked"]).unwrap().contains("T-005"));

        go(&d, &["close", "T-005"]).unwrap();
        let epic = std::fs::read_to_string(d.join("open/T-004-resolver-diverges.md")).unwrap();
        assert!(epic.contains("- [x] T-005"), "closing ticks the box: {epic}");
        assert!(go(&d, &["show", "T-004"]).unwrap().contains("children (1/1 done)"));

        go(&d, &["reopen", "T-005"]).unwrap();
        let epic = std::fs::read_to_string(d.join("open/T-004-resolver-diverges.md")).unwrap();
        assert!(epic.contains("- [ ] T-005"), "reopening unticks it: {epic}");
    }

    #[test]
    fn closing_an_epic_with_open_children_is_refused() {
        let d = fixture("epicclose");
        go(&d, &["new", "Big thing", "-p", "P1", "-s", "L", "-k", "epic"]).unwrap();
        go(&d, &["new", "Small thing", "-p", "P1", "-s", "S", "-e", "T-004"]).unwrap();

        let err = go(&d, &["close", "T-004"]).unwrap_err();
        let Error::Op(msg) = err else { panic!("expected an operational error") };
        assert!(msg.contains("T-005"), "names the open child: {msg}");
        assert!(d.join("open/T-004-big-thing.md").exists(), "epic stayed open");

        // --rejected drops the epic without demanding the children first.
        go(&d, &["close", "T-004", "--rejected"]).unwrap();
        assert!(d.join("closed/T-004-big-thing.md").exists());
    }

    #[test]
    fn epic_flag_rejects_a_non_epic_parent() {
        let d = fixture("epicbad");
        assert!(matches!(
            go(&d, &["new", "Child", "-p", "P1", "-s", "S", "-e", "T-001"]),
            Err(Error::Op(_)) // T-001 is a task
        ));
        assert!(matches!(
            go(&d, &["new", "Child", "-p", "P1", "-s", "S", "-e", "T-999"]),
            Err(Error::Op(_)) // no such ticket
        ));
        assert!(matches!(
            go(&d, &["new", "Child", "-p", "P1", "-s", "S", "-k", "upstream"]),
            Err(Error::Usage(_)) // upstream is a dossier, not a ticket kind
        ));
        // Neither attempt may leave a file behind.
        assert!(!d.join("open/T-004-child.md").exists());
    }

    #[test]
    fn upstream_indexes_dossiers_as_written() {
        let d = fixture("upstream");
        let up = d.join("../upstream");
        std::fs::create_dir_all(&up).unwrap();
        std::fs::write(
            up.join("ansible-vars_files.md"),
            "# Upstream regression dossier — a missing `vars_files` file is silently ignored\n\n\
             - [#80483](https://github.com/ansible/ansible/issues/80483) — filed by sivel\n\
             - [PR #80505](https://github.com/ansible/ansible/pull/80505) — closed unmerged\n",
        )
        .unwrap();
        std::fs::write(
            up.join("ansible-include_vars.md"),
            "# Upstream issues to file against ansible/ansible — `include_vars`\n\n\
             ## Issue 1 — ignore_files is treated as a regex\n\n\
             ## Issue 2 — the guard can never fire\n",
        )
        .unwrap();

        let out = go(&d, &["upstream"]).unwrap();
        assert!(out.contains("ansible-include_vars") && out.contains("2 issues"), "{out}");
        assert!(out.contains("not filed"), "nothing filed yet for include_vars: {out}");
        assert!(out.contains("#80483") && out.contains("PR#80505"), "{out}");
        assert!(out.contains("1 issue"), "single-topic dossier counts as one: {out}");

        let one = go(&d, &["upstream", "include_vars"]).unwrap();
        assert!(one.contains("1. ignore_files is treated as a regex"), "{one}");
        let _ = std::fs::remove_dir_all(&up);
    }

    /// The case `new -e` can't cover: by the time you know a ticket belongs in a group, it
    /// has usually been filed for months — and may already be closed.
    #[test]
    fn adopt_and_orphan_move_an_existing_ticket() {
        let d = fixture("adopt");
        go(&d, &["new", "Big thing", "-p", "P1", "-s", "L", "-k", "epic"]).unwrap();

        // An open ticket that predates kinds entirely: no Kind column, no Epic column.
        go(&d, &["adopt", "T-001", "-e", "T-004"]).unwrap();
        let child = std::fs::read_to_string(d.join("open/T-001-a.md")).unwrap();
        assert!(child.contains("| Epic"), "Epic column added: {child}");
        assert!(child.contains("| T-004"), "{child}");
        assert!(child.contains("| T-002"), "Depends on survived: {child}");
        let epic = std::fs::read_to_string(d.join("open/T-004-big-thing.md")).unwrap();
        assert!(epic.contains("- [ ] T-001 — Alpha"), "{epic}");

        // A closed ticket arrives ticked, so the list never claims shipped work is pending.
        go(&d, &["adopt", "T-003", "-e", "T-004"]).unwrap();
        let epic = std::fs::read_to_string(d.join("open/T-004-big-thing.md")).unwrap();
        assert!(epic.contains("- [x] T-003 — Gamma"), "closed arrives ticked: {epic}");
        assert!(go(&d, &["show", "T-004"]).unwrap().contains("children (1/2 done)"));

        go(&d, &["orphan", "T-001"]).unwrap();
        let child = std::fs::read_to_string(d.join("open/T-001-a.md")).unwrap();
        assert!(!child.contains("Epic"), "column removed: {child}");
        assert!(child.contains("| T-002"), "Depends on still there: {child}");
        let epic = std::fs::read_to_string(d.join("open/T-004-big-thing.md")).unwrap();
        assert!(!epic.contains("T-001"), "line dropped: {epic}");
        assert!(epic.contains("- [x] T-003"), "the other child stayed: {epic}");
    }

    /// Whether a ticket should have an epic is a judgement call, so this is a report and
    /// never an assertion — but the *data* has to be one command, or it gets grepped for
    /// by hand and the epics get counted as their own orphans.
    #[test]
    fn list_no_epic_reports_the_unparented() {
        let d = fixture("noepic");
        go(&d, &["new", "Big thing", "-p", "P1", "-s", "L", "-k", "epic"]).unwrap();
        go(&d, &["adopt", "T-001", "-e", "T-004"]).unwrap();

        let out = go(&d, &["list", "--no-epic"]).unwrap();
        assert!(!out.contains("T-001"), "adopted, so not orphaned: {out}");
        assert!(out.contains("T-002"), "unparented: {out}");
        assert!(!out.contains("T-004"), "an epic is not its own orphan: {out}");

        // Composes with the other filters rather than replacing them.
        let closed = go(&d, &["list", "--no-epic", "--closed"]).unwrap();
        assert!(closed.contains("T-003"), "closed and unparented: {closed}");
        assert!(!closed.contains("T-002"), "T-002 is open: {closed}");

        go(&d, &["adopt", "T-002", "-e", "T-004"]).unwrap();
        assert_eq!(go(&d, &["list", "--no-epic"]).unwrap(), "no tickets match\n");
    }

    #[test]
    fn adopt_refuses_the_ways_it_can_be_wrong() {
        let d = fixture("adoptbad");
        go(&d, &["new", "Big thing", "-p", "P1", "-s", "L", "-k", "epic"]).unwrap();
        go(&d, &["new", "Other epic", "-p", "P1", "-s", "L", "-k", "epic"]).unwrap();

        let op = |a: &[&str]| matches!(go(&d, a), Err(Error::Op(_)));
        assert!(op(&["adopt", "T-001", "-e", "T-002"]), "T-002 is not an epic");
        assert!(op(&["adopt", "T-004", "-e", "T-005"]), "epics don't nest");
        assert!(op(&["adopt", "T-004", "-e", "T-004"]), "cannot be its own epic");
        assert!(op(&["adopt", "T-999", "-e", "T-004"]), "no such ticket");
        assert!(op(&["orphan", "T-001"]), "not under an epic");
        assert!(matches!(go(&d, &["adopt", "T-001"]), Err(Error::Usage(_))), "needs --epic");

        go(&d, &["adopt", "T-001", "-e", "T-004"]).unwrap();
        assert!(op(&["adopt", "T-001", "-e", "T-004"]), "already there");
        assert!(op(&["adopt", "T-001", "-e", "T-005"]), "must orphan before moving");
        // A refused adopt must not have touched the epic it was aimed at.
        let other = std::fs::read_to_string(d.join("open/T-005-other-epic.md")).unwrap();
        assert!(!other.contains("T-001"), "{other}");
    }

    /// `close`/`reopen` rewrite the status cell; adopting adds a column. Neither may
    /// disturb the other's cells.
    #[test]
    fn header_rewrites_survive_each_other() {
        let d = fixture("hdr");
        go(&d, &["new", "Big thing", "-p", "P1", "-s", "L", "-k", "epic"]).unwrap();
        go(&d, &["adopt", "T-001", "-e", "T-004"]).unwrap();
        go(&d, &["close", "T-001"]).unwrap();

        let child = std::fs::read_to_string(d.join("closed/T-001-a.md")).unwrap();
        assert!(child.contains("| done"), "status flipped: {child}");
        assert!(child.contains("| T-004"), "epic survived the close: {child}");
        assert!(child.contains("| T-002"), "depends survived: {child}");
        let epic = std::fs::read_to_string(d.join("open/T-004-big-thing.md")).unwrap();
        assert!(epic.contains("- [x] T-001"), "close ticked the adopted child: {epic}");

        // `**rejected**` keeps its emphasis through a table rebuild.
        go(&d, &["adopt", "T-002", "-e", "T-004"]).unwrap();
        go(&d, &["close", "T-002", "--rejected"]).unwrap();
        let r = std::fs::read_to_string(d.join("closed/T-002-b.md")).unwrap();
        assert!(r.contains("**rejected**"), "emphasis survived: {r}");
    }

    #[test]
    fn show_prints_blocker_state_and_boxes() {
        let d = fixture("show");
        let out = go(&d, &["show", "T-001"]).unwrap();
        assert!(out.contains("T-002 (open)"), "{out}");
        assert!(out.contains("[ ] a thing"), "{out}");
    }
}
