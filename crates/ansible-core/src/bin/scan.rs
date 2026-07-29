//! Resolve every reference in a tree and report what didn't land.
//!
//! Doubles as the CI check: non-zero exit when literal file paths are missing.

use ansible_core::parse::Document;
use ansible_core::references::{extract, ReferenceKind};
use ansible_core::resolve::{resolve, SkipReason, Status};
use ansible_core::workspace::FileContext;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn yaml_files(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if p.is_dir() {
            if !matches!(name.as_str(), ".git" | "__pycache__" | ".pytest_cache" | "node_modules") {
                yaml_files(&p, out);
            }
        } else if matches!(
            p.extension().and_then(|s| s.to_str()),
            Some("yml") | Some("yaml")
        ) {
            out.push(p);
        }
    }
}

fn kind_name(k: ReferenceKind) -> &'static str {
    match k {
        ReferenceKind::IncludeTasks => "include_tasks",
        ReferenceKind::ImportTasks => "import_tasks",
        ReferenceKind::Role => "role",
        ReferenceKind::TasksFrom => "tasks_from",
        ReferenceKind::Module => "module",
    }
}

fn main() {
    let root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    let mut files = Vec::new();
    yaml_files(&root, &mut files);

    let mut totals: BTreeMap<&str, [usize; 3]> = BTreeMap::new(); // resolved, missing, skipped
    let mut missing: Vec<String> = Vec::new();
    let mut unresolved_roles: Vec<String> = Vec::new();
    let mut unparseable = 0usize;

    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let doc = Document::new(text);
        let Some(nodes) = doc.parse() else {
            unparseable += 1;
            continue;
        };
        let ctx = FileContext::discover(path);
        for r in extract(&nodes) {
            let res = resolve(&r, &ctx);
            let entry = totals.entry(kind_name(r.kind)).or_default();
            let rel = path.strip_prefix(&root).unwrap_or(path).display();
            let (line, _) = doc.byte_to_lsp(r.span.start);
            match res.status {
                Status::Resolved => entry[0] += 1,
                Status::Missing => {
                    entry[1] += 1;
                    missing.push(format!("{rel}:{}  {}", line + 1, r.value));
                }
                Status::Skipped => {
                    entry[2] += 1;
                    // A role name we couldn't place: is it really "installed
                    // elsewhere", or just wrong?
                    if r.kind == ReferenceKind::Role
                        && res.skip_reason == Some(SkipReason::NotInWorkspace)
                    {
                        unresolved_roles.push(format!("{rel}:{}  {}", line + 1, r.value));
                    }
                }
            }
        }
    }

    println!("{} files, {unparseable} unparseable\n", files.len());
    println!("{:<16} {:>9} {:>8} {:>8}", "kind", "resolved", "missing", "skipped");
    for (k, [r, m, s]) in &totals {
        println!("{k:<16} {r:>9} {m:>8} {s:>8}");
    }

    if !missing.is_empty() {
        println!("\nMISSING FILES ({}):", missing.len());
        for m in &missing {
            println!("  {m}");
        }
    }

    if !unresolved_roles.is_empty() {
        println!("\nUNRESOLVED ROLE NAMES ({}):", unresolved_roles.len());
        for r in &unresolved_roles {
            println!("  {r}");
        }
    }

    std::process::exit(if missing.is_empty() { 0 } else { 1 });
}
