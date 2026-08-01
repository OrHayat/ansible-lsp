//! Resolve every reference in a tree and report what didn't land.
//!
//! Doubles as the CI check: non-zero exit when literal file paths are missing.

use ansible_core::condition;
use ansible_core::mutation;
use ansible_core::parse::Document;
use ansible_core::references::{extract, ReferenceKind};
use ansible_core::resolve::{resolve, rule_id, SkipReason, Status};
use ansible_core::workspace::{yaml_files, FileContext};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn kind_name(k: ReferenceKind) -> &'static str {
    match k {
        ReferenceKind::IncludeTasks => "include_tasks",
        ReferenceKind::ImportTasks => "import_tasks",
        ReferenceKind::Role => "role",
        ReferenceKind::TasksFrom => "tasks_from",
        ReferenceKind::Module => "module",
        ReferenceKind::ImportPlaybook => "import_playbook",
        ReferenceKind::IncludeVars => "include_vars",
    }
}

fn main() {
    let root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    let files = yaml_files(&root);

    let mut totals: BTreeMap<&str, [usize; 3]> = BTreeMap::new(); // resolved, missing, skipped
    let mut missing: Vec<String> = Vec::new();
    let mut unresolved_roles: Vec<String> = Vec::new();
    let mut unparseable: Vec<String> = Vec::new();
    let mut mutated: Vec<String> = Vec::new();
    let mut empty_glob: Vec<String> = Vec::new();
    let mut tmpl_vars: BTreeMap<String, usize> = BTreeMap::new();
    let mut broken_when: Vec<String> = Vec::new();
    let mut mut_cache: BTreeMap<PathBuf, std::collections::HashSet<String>> = BTreeMap::new();

    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let doc = Document::new(text);
        let Some(nodes) = doc.parse() else {
            let rel = path.strip_prefix(&root).unwrap_or(path).display();
            match doc.parse_error() {
                Some(span) => {
                    let (line, _) = doc.byte_to_lsp(span.start);
                    unparseable.push(format!("{rel}:{}", line + 1));
                }
                None => unparseable.push(rel.to_string()),
            }
            continue;
        };
        let ctx = FileContext::discover(path);
        for r in extract(&nodes) {
            let res = resolve(&r, &ctx);

            // Conditions that cannot work at all, and the cross-file one.
            if let Some(span) = r.condition_span {
                let (cl, _) = doc.byte_to_lsp(span.start);
                for c in &r.conditions {
                    for p in condition::problems(c, r.repeated) {
                        if !doc.is_suppressed(span.start, p.rule_id()) {
                            broken_when.push(format!(
                                "  {}:{}  {}",
                                path.strip_prefix(&root).unwrap_or(path).display(),
                                cl + 1,
                                p.rule_id()
                            ));
                        }
                    }
                }
                if r.kind == ReferenceKind::ImportPlaybook
                    && !doc.is_suppressed(span.start, "when-import-var-mutated")
                {
                    let used: Vec<String> = r
                        .conditions
                        .iter()
                        .flat_map(|c| condition::variables(c))
                        .collect();
                    for target in &res.targets {
                        let m = mut_cache
                            .entry(target.clone())
                            .or_insert_with(|| mutation::mutated_vars(target));
                        let hit: Vec<&String> = used.iter().filter(|v| m.contains(*v)).collect();
                        if !hit.is_empty() {
                            mutated.push(format!(
                                "  {}:{}  {:?} set by {}",
                                path.strip_prefix(&root).unwrap_or(path).display(),
                                cl + 1,
                                hit,
                                target.strip_prefix(&root).unwrap_or(target).display()
                            ));
                        }
                    }
                }
            }

            // Templated, and the pattern reaches nothing on disk. Not warned about —
            // this list exists to judge whether that silence is right.
            if r.templated
                && res.status == Status::Skipped
                && matches!(r.kind, ReferenceKind::IncludeTasks | ReferenceKind::ImportTasks)
            {
                let (l, _) = doc.byte_to_lsp(r.span.start);
                empty_glob.push(format!(
                    "  {}:{}  {}",
                    path.strip_prefix(&root).unwrap_or(path).display(),
                    l + 1,
                    r.value
                ));
            }
            if r.templated {
                let mut rest = r.value.as_str();
                while let Some(i) = rest.find("{{") {
                    let after = &rest[i + 2..];
                    let Some(j) = after.find("}}") else { break };
                    let expr = after[..j].trim();
                    // The root identifier of the expression, which is what a
                    // substitution would have to know.
                    let root: String = expr
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    if !root.is_empty() {
                        *tmpl_vars.entry(root).or_default() += 1;
                    }
                    rest = &after[j + 2..];
                }
            }
            let entry = totals.entry(kind_name(r.kind)).or_default();
            let rel = path.strip_prefix(&root).unwrap_or(path).display();
            let (line, _) = doc.byte_to_lsp(r.span.start);
            match res.status {
                Status::Resolved => entry[0] += 1,
                Status::Missing if doc.is_suppressed(r.span.start, rule_id(&r)) => entry[2] += 1,
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

    println!("{} files, {} unparseable\n", files.len(), unparseable.len());
    println!("{:<16} {:>9} {:>8} {:>8}", "kind", "resolved", "missing", "skipped");
    for (k, [r, m, s]) in &totals {
        println!("{k:<16} {r:>9} {m:>8} {s:>8}");
    }

    if !unparseable.is_empty() {
        println!("\nUNPARSEABLE ({}):", unparseable.len());
        for f in &unparseable {
            println!("  {f}");
        }
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
    if !tmpl_vars.is_empty() {
        let mut v: Vec<_> = tmpl_vars.iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        println!("\nVARIABLES USED IN TEMPLATED PATHS ({}):", v.len());
        for (name, n) in v {
            println!("  {n:4}  {name}");
        }
    }

    if !empty_glob.is_empty() {
        println!("\nTEMPLATED, MATCHES NOTHING ({}):", empty_glob.len());
        for l in &empty_glob {
            println!("{l}");
        }
    }

    if !broken_when.is_empty() {
        println!("\nBROKEN `when:` ({}):", broken_when.len());
        for l in &broken_when {
            println!("{l}");
        }
    }

    // The condition is copied onto every imported task, so a `set_fact` inside the
    // import flips it mid-run and the playbook half-executes.
    if !mutated.is_empty() {
        println!("\nCONDITION VARIABLE MUTATED BY THE IMPORT ({}):", mutated.len());
        for l in &mutated {
            println!("{l}");
        }
    }


    std::process::exit(if missing.is_empty() { 0 } else { 1 });
}
