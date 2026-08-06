# T-011 — Execution tree via LSP call hierarchy

| Status       | Priority | Size | Commits                  | Epic  |
| ------------ | -------- | ---- | ------------------------ | ----- |
| **rejected** | P3       | L    | 9349187, reverted (-115) | T-113 |

## What was wanted

*"No way to give a playbook an execution context to see what will be called."* Reading a
playbook tells you nothing about what actually runs — plays -> roles -> tasks -> nested
includes is a dozen files of manual tracing.

## Why call hierarchy was chosen

The expansion genuinely *is* an outgoing-call tree, and both VS Code and Neovim render call
hierarchy natively — so the traversal UI, cycle handling and lazy expansion came free and
would have survived the move to vim (T-026) with no extra work.

## Why it was rejected

It worked at the protocol level and was still wrong. *"It's weird and it lies too."*

`prepareCallHierarchy` ignored the cursor. Asking about `ansible.builtin.fail` answered with
the whole playbook's 20 role calls — the same answer for every position in the file.

That is not a bug to patch. **Call hierarchy is symbol-scoped; this data is file-scoped.**
The protocol's contract is "resolve the symbol under the cursor, then show its callees."
There is no symbol under an Ansible task, so there is nothing for `prepare` to anchor to, and
any answer it gives will overstate its own scope. Making the response honest means not using
this protocol.

Removed in full, 115 lines. Two things were deliberately kept:

- the `when:` / `loop:` / task-name capture in `references.rs` — tested, and T-024 needs it
- the finding above, which is the whole reason T-024 is shaped the way it is

Successor: **T-024**, an explicit command feeding a sidebar TreeView rooted at a chosen
playbook, so its scope is stated rather than implied.
