// T-202 probe: does a multi-root workspace answer from the wrong folder?
//
// Paste into the `mod tests` block of crates/ansible-lsp/src/main.rs (it reuses the T-201
// helpers `T201_PLAY` and `t201_hover` that live there) and run:
//   cargo test -p ansible-lsp t202_multiroot_probe -- --nocapture
//
// Rewritten after T-201 removed INVENTORY_SETTING and WORKSPACE_ROOT. The earlier version
// wrote both globals and therefore poisoned the rest of the suite — with it present the
// ansible-lsp suite failed 6/10 runs (restoring the globals at the end) or 10/10 (not
// restoring). It now writes nothing process-wide, so it is safe to keep in a run; it stays a
// scratchpad probe only because it *prints* rather than asserts. T-202 owns turning it into
// assertions, which cannot happen until T-202 decides what the right answer is.
//
// Last run 2026-08-22, after T-201: folder B's file still answers `control` = `11` from
// folder A's inv.ini. Unchanged by T-201, which is the intended outcome — T-201 moved the
// value out of a global without touching the roots.first() resolution rule.

/// T-202: with two workspace folders, a relative `ansibleLsp.inventory` resolves against
/// folder #1 for files in every folder.
#[test]
fn t202_multiroot_probe() {
    let mk = |name: &str, val: &str| {
        ansible_core::testing::project(
            name,
            "[defaults]\n",
            &[
                ("inv.ini", &format!("[web]\nnode1 control={val}\n")),
                ("vars/11.yml", "x: 1\n"),
                ("vars/22.yml", "x: 1\n"),
                ("play.yml", T201_PLAY),
            ],
        )
    };
    let a = mk("t202-probe-a", "11");
    let b = mk("t202-probe-b", "22");

    // The window, in the order VS Code sent the folders.
    let window = |roots: Vec<std::path::PathBuf>| -> Vec<std::path::PathBuf> {
        let state = super::State {
            docs: Default::default(),
            roots: std::sync::Mutex::new(roots),
            flagged: Default::default(),
            mutations: Default::default(),
            settings: Default::default(),
            inventory: Default::default(),
            startup_note: Default::default(),
            scanning: std::sync::atomic::AtomicBool::new(false),
        };
        state.set_inventory(&serde_json::json!({ "inventory": ["inv.ini"] }));
        state.inventory_setting()
    };

    let ab = window(vec![a.clone(), b.clone()]);
    println!("roots=[A, B] -> {ab:?}");
    println!("  file in A:\n{}", t201_hover(&a, &ab));
    println!("  file in B:\n{}", t201_hover(&b, &ab));

    // The control: reversing the folder order must flip both answers. Without it, "folder B
    // says 11" is also what you would see if the inventory were never read at all.
    let ba = window(vec![b.clone(), a.clone()]);
    println!("roots=[B, A] -> {ba:?}");
    println!("  file in B:\n{}", t201_hover(&b, &ba));
    println!("  file in A:\n{}", t201_hover(&a, &ba));

    // Not yet measured, and T-202's real open question: B nested *inside* A, which VS Code
    // allows. Add a third fixture at a.join("sub") with its own inv.ini and ask about a file
    // in it under roots=[A, A/sub] — "longest containing root wins" is only a candidate rule,
    // and it reads no inventory at all when A/sub/inv.ini does not exist.
}
