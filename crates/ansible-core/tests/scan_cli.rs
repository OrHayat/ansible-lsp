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
