//! The `board` CLI's contract, driven as a subprocess against a throwaway board.
//!
//! The exit code is the part that matters and the part a unit test cannot see: `0` ok,
//! `1` bad usage, `2` operation failed. Scripts and the agent instructions both read it, so
//! a command that started returning `0` on failure would go unnoticed until a broken board
//! was already committed.
//!
//! `--dir` exists for this — every test gets its own board and none of them can touch the
//! real `tasks/`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const README: &str = "\
# Tasks

## Open

### P1 — the tool lies or goes silent

| ID | Title | Size | Blocked by |
| -- | ----- | ---- | ---------- |

### P2 — coverage and usability

| ID | Title | Size | Refs |
| -- | ----- | ---- | ---- |

### P3 — nice to have

| ID | Title | Size | Refs |
| -- | ----- | ---- | ---- |

## Closed

| ID | Title | Outcome |
| -- | ----- | ------- |
";

fn board_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("open")).unwrap();
    std::fs::create_dir_all(d.join("closed")).unwrap();
    std::fs::write(d.join("README.md"), README).unwrap();
    d
}

fn run(d: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_board"))
        .arg("--dir")
        .arg(d)
        .args(args)
        .output()
        .expect("the board binary runs")
}

fn code(o: &Output) -> i32 {
    o.status.code().expect("exited normally")
}

fn text(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr)
}

/// The three exit codes, each from a command that reaches it a different way.
///
/// Without the `0` row the other two prove only that the binary can fail, which a `exit(1)`
/// at the top of `main` would also satisfy.
#[test]
fn the_exit_code_separates_usage_from_failure_from_success() {
    let d = board_dir("board-cli-exits");

    assert_eq!(code(&run(&d, &["list"])), 0, "an empty board is not an error");
    assert_eq!(code(&run(&d, &["help"])), 0);

    // 1 — the command line itself is wrong. Each of these is a separate validation.
    assert_eq!(code(&run(&d, &["bogus"])), 1, "unknown subcommand");
    assert_eq!(code(&run(&d, &["new", "T", "-p", "P9", "-s", "S"])), 1, "bad priority");
    assert_eq!(code(&run(&d, &["new", "T", "-p", "P1", "-s", "XL"])), 1, "bad size");
    assert_eq!(code(&run(&d, &["new", "T", "-p", "P1", "-s", "S", "-b", "nope"])), 1, "bad id");
    assert_eq!(code(&run(&d, &["show"])), 1, "a required argument missing");
    assert_eq!(code(&run(&d, &["list", "--wat"])), 1, "unexpected flag");

    // 2 — the command was well formed and the operation could not be done.
    assert_eq!(code(&run(&d, &["show", "T-999"])), 2, "no such ticket");
    assert_eq!(code(&run(&d, &["close", "T-999"])), 2);
    assert_eq!(code(&run(&d, &["reopen", "T-999"])), 2);

    // Each usage error explains itself rather than just failing.
    let out = run(&d, &["new", "T", "-p", "P9", "-s", "S"]);
    assert!(text(&out).contains("P1|P2|P3"), "names the valid set: {}", text(&out));
    assert!(text(&out).contains("P9"), "and what was given: {}", text(&out));
}

/// A ticket's whole life, with the folder as the status at every step — the property the
/// board's README states and the CLI exists to keep true.
#[test]
fn a_ticket_moves_between_folders_and_the_readme_follows() {
    let d = board_dir("board-cli-lifecycle");

    let out = run(&d, &["new", "A first ticket", "-p", "P1", "-s", "S"]);
    assert_eq!(code(&out), 0, "{}", text(&out));
    let id = "T-001";

    let open_file = |id: &str| {
        std::fs::read_dir(d.join("open"))
            .unwrap()
            .flatten()
            .find(|e| e.file_name().to_string_lossy().starts_with(id))
    };
    assert!(open_file(id).is_some(), "a new ticket lands in open/");
    let readme = std::fs::read_to_string(d.join("README.md")).unwrap();
    assert!(readme.contains(id), "and gets a README row");

    // Closing moves the file and rewrites the row; the id is never reused.
    assert_eq!(code(&run(&d, &["close", id])), 0);
    assert!(open_file(id).is_none(), "closed tickets leave open/");
    assert!(
        std::fs::read_dir(d.join("closed")).unwrap().flatten().any(|e| e
            .file_name()
            .to_string_lossy()
            .starts_with(id)),
        "and arrive in closed/"
    );
    let readme = std::fs::read_to_string(d.join("README.md")).unwrap();
    assert!(readme.contains("## Closed"), "the closed section survives");

    // Reopening is the inverse.
    assert_eq!(code(&run(&d, &["reopen", id])), 0);
    assert!(open_file(id).is_some(), "reopen puts it back");

    // Ids are never reused, even across a close.
    assert_eq!(code(&run(&d, &["new", "A second", "-p", "P2", "-s", "M"])), 0);
    assert!(open_file("T-002").is_some(), "the next id is T-002, not a reused T-001");
}

/// `--dry-run` is the flag a caller trusts to be safe, so "writes nothing" is the assertion —
/// not that it prints a plan.
#[test]
fn dry_run_prints_the_plan_and_writes_nothing() {
    let d = board_dir("board-cli-dryrun");
    let before = std::fs::read_dir(d.join("open")).unwrap().flatten().count();
    let readme_before = std::fs::read_to_string(d.join("README.md")).unwrap();

    let out = run(&d, &["--dry-run", "new", "Nothing doing", "-p", "P1", "-s", "S"]);
    assert_eq!(code(&out), 0, "{}", text(&out));

    let after = std::fs::read_dir(d.join("open")).unwrap().flatten().count();
    assert_eq!(after, before, "no file was created");
    assert_eq!(
        std::fs::read_to_string(d.join("README.md")).unwrap(),
        readme_before,
        "and the README is byte-identical"
    );
}

/// Filters compose, and each one actually narrows. A filter that returned everything would
/// satisfy a test that only checked the matching row is present.
#[test]
fn list_filters_narrow_rather_than_decorate() {
    let d = board_dir("board-cli-filters");
    run(&d, &["new", "A p one small", "-p", "P1", "-s", "S"]);
    run(&d, &["new", "A p two large", "-p", "P2", "-s", "L"]);
    run(&d, &["new", "A bug row", "-p", "P1", "-s", "M", "-k", "bug"]);

    let listed = |args: &[&str]| text(&run(&d, args));

    let all = listed(&["list"]);
    assert!(all.contains("T-001") && all.contains("T-002") && all.contains("T-003"));

    let p1 = listed(&["list", "-p", "P1"]);
    assert!(p1.contains("T-001") && p1.contains("T-003"), "both P1s: {p1}");
    assert!(!p1.contains("T-002"), "the P2 is excluded: {p1}");

    let small = listed(&["list", "-s", "S"]);
    assert!(small.contains("T-001") && !small.contains("T-002"), "{small}");

    let bugs = listed(&["list", "-k", "bug"]);
    assert!(bugs.contains("T-003") && !bugs.contains("T-001"), "{bugs}");

    // Combined, they intersect rather than union.
    let both = listed(&["list", "-p", "P1", "-s", "M"]);
    assert!(both.contains("T-003"), "{both}");
    assert!(!both.contains("T-001"), "P1 but not M must be excluded: {both}");
}

/// `-b` records a real gate and `--unblocked` respects it; closing the blocker releases it.
#[test]
fn blocked_by_gates_the_unblocked_list_until_the_blocker_closes() {
    let d = board_dir("board-cli-blocked");
    run(&d, &["new", "The blocker", "-p", "P1", "-s", "S"]);
    run(&d, &["new", "The blocked", "-p", "P1", "-s", "S", "-b", "T-001"]);

    let unblocked = text(&run(&d, &["list", "--unblocked"]));
    assert!(unblocked.contains("T-001"), "the blocker itself is workable: {unblocked}");
    assert!(!unblocked.contains("T-002"), "the blocked one is not: {unblocked}");

    assert_eq!(code(&run(&d, &["close", "T-001"])), 0);
    let unblocked = text(&run(&d, &["list", "--unblocked"]));
    assert!(unblocked.contains("T-002"), "closing the blocker releases it: {unblocked}");
}

/// An epic refuses to close while a child is open, and names the child — the guard that
/// stops an epic being ticked off over unfinished work.
#[test]
fn an_epic_will_not_close_over_an_open_child() {
    let d = board_dir("board-cli-epic");
    assert_eq!(code(&run(&d, &["new", "The epic", "-p", "P1", "-s", "L", "-k", "epic"])), 0);
    assert_eq!(code(&run(&d, &["new", "A child", "-p", "P1", "-s", "S", "-e", "T-001"])), 0);

    let out = run(&d, &["close", "T-001"]);
    assert_eq!(code(&out), 2, "refused while the child is open");
    assert!(text(&out).contains("T-002"), "and names it: {}", text(&out));

    // `--rejected` drops it anyway, for an epic that turned out to be the wrong framing.
    assert_eq!(code(&run(&d, &["close", "T-001", "--rejected"])), 0);

    // An epic reference must exist and be an epic — both checked before anything is written.
    assert_eq!(code(&run(&d, &["new", "X", "-p", "P1", "-s", "S", "-e", "T-999"])), 2);
    assert_eq!(
        code(&run(&d, &["new", "X", "-p", "P1", "-s", "S", "-e", "T-002"])),
        2,
        "T-002 is a task, not an epic"
    );
}
