//! The `scan` binary's contract, which is a CI gate and had no test at all.
//!
//! Driven as a subprocess rather than by calling into it, because the thing being asserted
//! is the *exit code* — the part CI reads — and that lives in `std::process::exit`, which a
//! unit test cannot observe. `CARGO_BIN_EXE_scan` is the binary this build produced, so the
//! test can never drift onto a stale one.

use std::path::{Path, PathBuf};
use std::process::Command;

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn tree(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    write(&d, "ansible.cfg", "[defaults]\nroles_path = ./roles\n");
    d
}

fn scan(root: &Path) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_scan"))
        .arg(root)
        .output()
        .expect("the scan binary runs");
    let text = String::from_utf8_lossy(&out.stdout).to_string()
        + &String::from_utf8_lossy(&out.stderr);
    (out.status.success(), text)
}

/// The gate itself: a tree whose references all land exits 0, and one literal missing file
/// turns it red.
///
/// Both halves matter. A gate that never fails is the failure mode CLAUDE.md rule 2 is about
/// — this is the CI command, and a green it cannot lose is worse than no gate.
#[test]
fn the_exit_code_is_the_ci_gate() {
    let d = tree("ansible-lsp-scan-gate");
    write(&d, "tasks/real.yml", "- ansible.builtin.debug:\n    msg: hi\n");
    write(
        &d,
        "play.yml",
        "- hosts: all\n  tasks:\n    - ansible.builtin.import_tasks: tasks/real.yml\n",
    );

    let (ok, text) = scan(&d);
    assert!(ok, "everything resolves, so the gate is green:\n{text}");
    assert!(text.contains("import_tasks"), "the kind table names the edge:\n{text}");

    // One literal path that is not there, and nothing else changed.
    write(
        &d,
        "play.yml",
        "- hosts: all\n  tasks:\n    - ansible.builtin.import_tasks: tasks/real.yml\n    \
         - ansible.builtin.import_tasks: tasks/gone.yml\n",
    );
    let (ok, text) = scan(&d);
    assert!(!ok, "a missing literal path must fail the gate:\n{text}");
    assert!(text.contains("MISSING FILES"), "and say what is missing:\n{text}");
    assert!(text.contains("gone.yml"), "naming the file:\n{text}");
}

/// A *templated* path is not a missing file. This is the distinction the gate would be
/// useless without: half the corpus computes its include paths, and failing on those would
/// make the check unrunnable and it would be turned off.
#[test]
fn a_templated_path_does_not_fail_the_gate() {
    let d = tree("ansible-lsp-scan-templated");
    write(
        &d,
        "play.yml",
        "- hosts: all\n  tasks:\n    - ansible.builtin.include_tasks: \"{{ whatever }}.yml\"\n",
    );
    let (ok, text) = scan(&d);
    assert!(ok, "a computed path is unknown, not missing:\n{text}");
    assert!(!text.contains("MISSING FILES"), "{text}");
}

/// The report's shape: the counts line, and one table row per kind actually seen. Asserted
/// because these are what a human reads to decide whether a change moved anything.
#[test]
fn the_report_counts_files_and_breaks_down_by_kind() {
    let d = tree("ansible-lsp-scan-report");
    write(&d, "roles/r/tasks/main.yml", "- ansible.builtin.debug:\n    msg: hi\n");
    write(&d, "vars/v.yml", "k: 1\n");
    write(&d, "tasks/t.yml", "- ansible.builtin.debug:\n    msg: hi\n");
    write(
        &d,
        "play.yml",
        "- hosts: all\n  vars_files:\n    - vars/v.yml\n  roles:\n    - r\n  tasks:\n    \
         - ansible.builtin.include_tasks: tasks/t.yml\n",
    );

    let (ok, text) = scan(&d);
    assert!(ok, "{text}");
    // "N files, M unparseable" — the headline, and the parse count must see every file.
    let head = text.lines().find(|l| l.contains(" files,")).expect(&format!("headline:\n{text}"));
    assert!(head.contains("0 unparseable"), "this fixture is all valid YAML: {head}");
    // One row per kind that appeared, using the binary's own names.
    for kind in ["role", "vars_files", "include_tasks"] {
        assert!(
            text.lines().any(|l| l.split_whitespace().next() == Some(kind)),
            "no `{kind}` row in:\n{text}"
        );
    }
    // And no row for a kind nothing in the fixture uses.
    assert!(
        !text.lines().any(|l| l.split_whitespace().next() == Some("import_playbook")),
        "a kind with no references must not get a row:\n{text}"
    );
}

/// Unparseable files are counted and named rather than aborting the run — a scan that died
/// on the first bad file could never report the rest of a real tree.
#[test]
fn a_broken_file_is_reported_and_the_scan_continues() {
    let d = tree("ansible-lsp-scan-broken");
    write(&d, "tasks/real.yml", "- ansible.builtin.debug:\n    msg: hi\n");
    write(&d, "broken.yml", "- hosts: all\n  tasks:\n   - bad\n  indent: [\n");
    write(
        &d,
        "play.yml",
        "- hosts: all\n  tasks:\n    - ansible.builtin.import_tasks: tasks/real.yml\n",
    );

    let (_, text) = scan(&d);
    assert!(text.contains("UNPARSEABLE"), "the broken file is reported:\n{text}");
    assert!(text.contains("broken.yml"), "by name:\n{text}");
    // The good file was still walked — the scan did not stop at the bad one.
    assert!(text.contains("import_tasks"), "the rest of the tree still scanned:\n{text}");
}

/// No argument means the current directory, which is how the command is usually typed.
#[test]
fn the_root_defaults_to_the_working_directory() {
    let d = tree("ansible-lsp-scan-cwd");
    write(&d, "play.yml", "- hosts: all\n  tasks: []\n");
    let out = Command::new(env!("CARGO_BIN_EXE_scan"))
        .current_dir(&d)
        .output()
        .expect("runs with no argument");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains(" files,"), "scanned the cwd:\n{text}");
    assert!(out.status.success());
}

/// The report's **finding** sections, each from a tree that actually contains that fault.
///
/// These are the sections a human reads, and every one of them was dark: the other tests
/// here scan healthy trees plus one missing file, so the happy path and a single failure
/// were covered while the six kinds of finding the report can print were not.
#[test]
fn every_finding_section_prints_when_the_tree_earns_it() {
    let d = tree("ansible-lsp-scan-findings");

    // A `when:` that cannot work: `{{ }}` inside a condition, which ansible-core rejects.
    write(
        &d,
        "broken.yml",
        "- hosts: all\n  tasks:\n    - ansible.builtin.debug:\n        msg: hi\n      when: \"{{ flag }}\"\n",
    );
    // A role whose directory exists but has no `tasks/main.yml`, referenced with
    // `tasks_from`. That is the shape the report singles out: the role *is* there, so
    // "missing file" would be wrong, but nothing placed its entry point — the section asks
    // whether it is really installed elsewhere or simply wrong. It also gives the
    // `tasks_from` kind its row.
    write(&d, "roles/nomain/tasks/other.yml", "- ansible.builtin.debug:\n    msg: hi\n");
    write(
        &d,
        "from.yml",
        "- hosts: all\n  tasks:\n    - ansible.builtin.include_role:\n        name: nomain\n        tasks_from: other.yml\n",
    );

    let (_, text) = scan(&d);
    assert!(text.contains("BROKEN `when:`"), "the broken-condition section:\n{text}");
    assert!(text.contains("when-jinja-delimiters"), "naming the rule:\n{text}");
    assert!(text.contains("UNRESOLVED ROLE NAMES"), "the unresolved-role section:\n{text}");
    assert!(text.contains("nomain"), "naming the role:\n{text}");
    assert!(
        text.lines().any(|l| l.split_whitespace().next() == Some("tasks_from")),
        "the tasks_from kind row:\n{text}"
    );
}

/// The cross-file check: an import gated on a variable its own target sets. The condition is
/// evaluated before the target runs, so the gate never sees the value — a fault only a
/// two-file view can find, and the one report section that needs a second file to exist.
#[test]
fn a_condition_the_import_itself_mutates_is_reported() {
    let d = tree("ansible-lsp-scan-mutated");
    write(
        &d,
        "sets.yml",
        "- hosts: all\n  tasks:\n    - ansible.builtin.set_fact:\n        ready: true\n",
    );
    write(
        &d,
        "play.yml",
        "- ansible.builtin.import_playbook: sets.yml\n  when: ready | default(false)\n",
    );

    let (_, text) = scan(&d);
    assert!(
        text.contains("CONDITION VARIABLE MUTATED BY THE IMPORT"),
        "the cross-file section:\n{text}"
    );
    assert!(text.contains("ready"), "naming the variable:\n{text}");
    assert!(text.contains("sets.yml"), "and the file that sets it:\n{text}");
}

/// The templated-variables section ranks by how often each name is used, which is the only
/// reason it is a list rather than a set — it answers "which variable would a substitution
/// have to know first". One-variable fixtures never run the comparator, so the order was
/// never checked.
#[test]
fn templated_variables_are_ranked_by_how_often_they_appear() {
    let d = tree("ansible-lsp-scan-tmplvars");
    write(
        &d,
        "play.yml",
        "- hosts: all\n  tasks:\n    - ansible.builtin.include_tasks: \"{{ common }}.yml\"\n    - ansible.builtin.include_tasks: \"{{ common }}/x.yml\"\n    - ansible.builtin.include_tasks: \"{{ rare }}.yml\"\n",
    );

    let (ok, text) = scan(&d);
    assert!(ok, "{text}");
    let at = |name: &str| {
        text.lines()
            .position(|l| l.split_whitespace().nth(1) == Some(name))
            .unwrap_or_else(|| panic!("no `{name}` row in:\n{text}"))
    };
    assert!(text.contains("   2  common"), "counted twice:\n{text}");
    assert!(text.contains("   1  rare"), "and the other once:\n{text}");
    assert!(at("common") < at("rare"), "the commoner name is listed first:\n{text}");
}

/// A role's `meta/main.yml` dependencies are references too, and *only* from that filename.
///
/// The controls are the point: the same `dependencies:` block in the role's `vars/main.yml`
/// and in a `meta/other.yml` must be read by nobody. Without them this passes against a scan
/// that treats every `dependencies:` key anywhere as a role list.
///
/// The `ctx.role_dir.is_some()` half of the guard is not probed here because it cannot fail:
/// `is_role_dir` is "has a `tasks`, `defaults` or `meta` dir", so a file at `<d>/meta/main.yml`
/// always sits in something that answers yes. A control for it would be a test that cannot
/// come out the other way.
#[test]
fn a_role_meta_dependency_is_a_reference_and_only_from_main_yml() {
    let d = tree("ansible-lsp-scan-meta");
    write(&d, "roles/r/tasks/main.yml", "- ansible.builtin.debug:\n    msg: hi\n");
    write(&d, "roles/r/meta/main.yml", "dependencies:\n  - ghost-role\n");
    write(&d, "roles/r/vars/main.yml", "dependencies:\n  - ghost-in-vars\n");
    write(&d, "roles/r/meta/other.yml", "dependencies:\n  - ghost-in-other-meta\n");
    write(&d, "play.yml", "- hosts: all\n  roles:\n    - r\n");

    let (ok, text) = scan(&d);
    assert!(!ok, "a dependency on a role that isn't there fails the gate:\n{text}");
    assert!(text.contains("ghost-role"), "the meta dependency is followed:\n{text}");
    assert!(!text.contains("ghost-in-vars"), "`vars/main.yml` is not a meta file:\n{text}");
    assert!(!text.contains("ghost-in-other-meta"), "nor is `meta/other.yml`:\n{text}");
}

/// `include_vars` splits into two kinds by shape — a file and a `dir:` sweep resolve by
/// different rules, so they are counted apart. Asserting the whole row catches the dir form
/// being folded into the file count.
#[test]
fn include_vars_is_counted_apart_from_its_dir_form() {
    let d = tree("ansible-lsp-scan-includevars");
    write(&d, "vars/v.yml", "k: 1\n");
    write(
        &d,
        "play.yml",
        "- hosts: all\n  tasks:\n    - ansible.builtin.include_vars: vars/v.yml\n    - ansible.builtin.include_vars:\n        dir: vars\n",
    );

    let (ok, text) = scan(&d);
    assert!(ok, "{text}");
    let row = |kind: &str| {
        text.lines()
            .find(|l| l.split_whitespace().next() == Some(kind))
            .unwrap_or_else(|| panic!("no `{kind}` row in:\n{text}"))
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    assert_eq!(row("include_vars"), ["include_vars", "1", "0", "0"], "one file, resolved");
    assert_eq!(row("include_vars_dir"), ["include_vars_dir", "1", "0", "0"], "the dir, apart");
}

/// `# noqa` silences the finding it names and nothing else. Each half of this test is a pair:
/// an annotated site that must disappear and an identical one that must not, so a suppression
/// that had grown to swallow everything fails here.
#[test]
fn noqa_silences_the_finding_it_names_and_not_its_neighbour() {
    let d = tree("ansible-lsp-scan-noqa");
    write(
        &d,
        "play.yml",
        "- hosts: all\n  tasks:\n    - ansible.builtin.debug:\n        msg: \"{{ hushed_var }}\"  # noqa: var-undefined\n    - ansible.builtin.debug:\n        msg: \"{{ loud_var }}\"\n",
    );
    write(
        &d,
        "sets.yml",
        "- hosts: all\n  tasks:\n    - ansible.builtin.set_fact:\n        ready: true\n",
    );
    write(
        &d,
        "hushed.yml",
        "- ansible.builtin.import_playbook: sets.yml\n  when: ready | default(false)  # noqa: when-import-var-mutated\n",
    );
    write(
        &d,
        "loud.yml",
        "- ansible.builtin.import_playbook: sets.yml\n  when: ready | default(false)\n",
    );

    let (_, text) = scan(&d);
    assert!(text.contains("loud_var"), "the unannotated use is still reported:\n{text}");
    assert!(!text.contains("hushed_var"), "the annotated one is not:\n{text}");
    assert!(text.contains("loud.yml"), "the unannotated import is still reported:\n{text}");
    assert!(!text.contains("hushed.yml"), "the annotated one is not:\n{text}");
}

/// A file the walk finds but cannot read is **named**, and is not called unparseable.
///
/// Both halves are the bug this test was written for. The scan used to drop an unreadable file
/// silently while still counting it in the headline, so a tree whose only broken reference sat
/// in a file nobody could open reported a clean bill of health — see the second half below,
/// which is the whole reason this section exists rather than just a skip.
///
/// `unparseable` stays a separate count: it means "Ansible would choke on this too", while a
/// permissions failure says nothing at all about the YAML.
#[cfg(unix)]
#[test]
fn an_unreadable_file_is_named_rather_than_silently_dropped() {
    use std::os::unix::fs::PermissionsExt;

    let d = tree("ansible-lsp-scan-unreadable");
    write(&d, "tasks/real.yml", "- ansible.builtin.debug:\n    msg: hi\n");
    write(
        &d,
        "play.yml",
        "- hosts: all\n  tasks:\n    - ansible.builtin.import_tasks: tasks/real.yml\n",
    );
    let locked = d.join("locked.yml");
    std::fs::write(&locked, "- hosts: all\n  tasks: []\n").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&locked).is_ok() {
        return; // running as root, where the mode proves nothing
    }

    let (ok, text) = scan(&d);
    let _ = std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644));

    assert!(ok, "an unreadable file is not itself a missing reference:\n{text}");
    assert!(text.contains("3 files, 0 unparseable, 1 unreadable"), "counted apart:\n{text}");
    assert!(text.contains("UNREADABLE"), "and given its own section:\n{text}");
    assert!(text.contains("locked.yml"), "naming the file:\n{text}");
    assert!(text.contains("import_tasks"), "the rest of the tree still scanned:\n{text}");
}

/// The bug itself: an unreadable file used to hide whatever was inside it, including a broken
/// reference that fails the gate the moment the same file is readable.
///
/// One permissions bit apart, so the probe cannot pass by accident — the readable run must go
/// red and name the missing include, and the unreadable run must still say out loud that it
/// could not look. A green with no explanation is the failure this test exists to prevent.
#[cfg(unix)]
#[test]
fn an_unreadable_file_cannot_quietly_take_a_broken_reference_with_it() {
    use std::os::unix::fs::PermissionsExt;

    let d = tree("ansible-lsp-scan-hidden");
    let hides = d.join("hides.yml");
    std::fs::write(
        &hides,
        "- hosts: all\n  tasks:\n    - ansible.builtin.import_tasks: tasks/gone.yml\n",
    )
    .unwrap();

    let (ok, text) = scan(&d);
    assert!(!ok, "readable, the missing include fails the gate:\n{text}");
    assert!(text.contains("gone.yml"), "and is named:\n{text}");

    std::fs::set_permissions(&hides, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&hides).is_ok() {
        return; // running as root
    }
    let (_, text) = scan(&d);
    let _ = std::fs::set_permissions(&hides, std::fs::Permissions::from_mode(0o644));

    assert!(
        text.contains("UNREADABLE") && text.contains("hides.yml"),
        "unreadable, the scan must say it could not check the file rather than report \
         nothing and exit clean:\n{text}"
    );
}

/// The config header, in each of the three shapes it can take: a project root with an
/// `ansible.cfg`, no root at all, and an `ANSIBLE_CONFIG` override that beats both.
#[test]
fn the_config_header_names_which_file_was_used() {
    let d = tree("ansible-lsp-scan-config");
    write(&d, "play.yml", "- hosts: all\n  tasks: []\n");

    let (_, text) = scan(&d);
    assert!(text.contains("ansible.cfg"), "the project's own config:\n{text}");

    // An override is announced, because a config from the environment explains a resolution
    // nothing in the tree accounts for.
    let other = d.join("elsewhere.cfg");
    std::fs::write(&other, "[defaults]\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_scan"))
        .arg(&d)
        .env("ANSIBLE_CONFIG", &other)
        .output()
        .expect("runs with an override");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(text.contains("config override"), "the override is announced:\n{text}");
    assert!(text.contains("elsewhere.cfg"), "by name:\n{text}");

    // A tree with no `ansible.cfg` anywhere has no project root to report.
    let bare = std::env::temp_dir().join("ansible-lsp-scan-noroot");
    let _ = std::fs::remove_dir_all(&bare);
    std::fs::create_dir_all(&bare).unwrap();
    write(&bare, "play.yml", "- hosts: all\n  tasks: []\n");
    let (_, text) = scan(&bare);
    assert!(text.contains("no project root"), "says so rather than inventing one:\n{text}");
}

// ---- T-194: the scan and the editor answer the same question ---------------------------

/// The lines under a named section heading, up to the blank line that ends it. Returned
/// rather than searched for as a substring because "does this string appear in the report"
/// cannot tell a path listed under `TEMPLATED, MATCHES NOTHING` from the same path listed
/// under `MISSING FILES`, and this rule is about which of the two a reference lands in.
fn section(text: &str, heading: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if line.starts_with(heading) {
            inside = true;
            continue;
        }
        if inside {
            if line.trim().is_empty() {
                break;
            }
            out.push(line.trim().to_string());
        }
    }
    out
}

/// One row of the `kind resolved missing skipped` table, as three numbers.
fn kind_row(text: &str, kind: &str) -> [usize; 3] {
    let line = text
        .lines()
        .find(|l| l.split_whitespace().next() == Some(kind))
        .unwrap_or_else(|| panic!("the report has a `{kind}` row:\n{text}"));
    let n: Vec<usize> =
        line.split_whitespace().skip(1).filter_map(|f| f.parse().ok()).collect();
    [n[0], n[1], n[2]]
}

/// T-194: a templated path whose variable is a plain literal is *knowable*, and the editor
/// navigates it — `main.rs` builds a `Resolver` with `vars::known_literals_in`. The scan
/// calls `resolve_in`, which takes no literals, so it printed the same reference under
/// `TEMPLATED, MATCHES NOTHING`. One question, two consumers, two answers: rule 3.
///
/// The second include is the control. It has no literal anywhere, so it must *stay* in that
/// section — otherwise this test would pass just as well against a scan that stopped
/// reporting templated paths at all, which is the failure mode rule 2 is about.
#[test]
fn the_scan_substitutes_a_known_literal_into_a_templated_path() {
    let d = tree("ansible-lsp-scan-known-literal");
    write(&d, "roles/r/defaults/main.yml", "chosen_task: chosen.yml\n");
    write(
        &d,
        "roles/r/tasks/chosen.yml",
        "- ansible.builtin.debug:\n    msg: picked\n",
    );
    // In `tasks/main.yml` rather than a file beside it, so the role itself resolves from
    // `play.yml` — a role with no default entry point is `missing`, and that fault would
    // land in the same report as the one being measured.
    write(
        &d,
        "roles/r/tasks/main.yml",
        "- ansible.builtin.include_tasks: \"{{ chosen_task }}\"\n\
         - ansible.builtin.include_tasks: \"{{ never_defined_anywhere }}.yml\"\n",
    );
    write(&d, "play.yml", "- hosts: all\n  roles:\n    - r\n");

    let (ok, text) = scan(&d);

    // A substituted path is navigable, never warned about: `resolve_with` stamps
    // `SkipReason::Templated` and turns Missing into Skipped. The gate cannot move.
    assert!(ok, "substituting a literal must not fail the CI gate:\n{text}");
    assert!(!text.contains("MISSING FILES"), "and must not invent a missing file:\n{text}");

    let empty_glob = section(&text, "TEMPLATED, MATCHES NOTHING");
    assert!(
        empty_glob.iter().any(|l| l.contains("never_defined_anywhere")),
        "control: a variable with no literal is still unknowable:\n{text}"
    );
    assert!(
        !empty_glob.iter().any(|l| l.contains("chosen_task")),
        "the literal is in the role's own defaults, so the path is knowable — the editor \
         navigates it and the scan must not call it unmatched:\n{text}"
    );

    let [resolved, _, _] = kind_row(&text, "include_tasks");
    assert_eq!(resolved, 1, "and it counts as resolved, not skipped:\n{text}");
}

/// Rule 4: `demo/include_vars_demo.yml` labels its `"vars/{{ env }}.yml"` row
/// "Templated but knowable — navigates to vars/prod.yml". That label is a claim about the
/// tool, and nothing pinned it — the scan answered `Skipped` for the same reference while
/// the comment said it resolves.
///
/// Pinned on the counts table because a skipped `include_vars` is not printed line by line.
/// The neighbouring row is the control: `"vars/{{ region }}.yml"` has no definition anywhere
/// (the demo's own `UNDEFINED VARIABLES` list names `region`), so exactly one of the two
/// moves.
#[test]
fn the_demo_s_knowable_include_vars_path_resolves_for_the_scan_too() {
    let demo = std::path::Path::new("../../demo").canonicalize().unwrap();
    let (_, text) = scan(&demo);

    let [resolved, _, skipped] = kind_row(&text, "include_vars");
    assert_eq!(
        skipped, 1,
        "only `vars/{{{{ region }}}}` is unknowable; `vars/{{{{ env }}}}` has env: prod \
         one file away:\n{text}"
    );
    // Four for `include_vars_demo.yml` — three literal paths plus the knowable templated one
    // — and a fifth for chain-c's re-include of its own `vars/main.yml`, the T-207 fixture.
    assert_eq!(resolved, 5, "so the knowable one joins the four literal paths:\n{text}");
}
