# T-232 — The server detects the Ansible install but never stores it, so the editor answers as if Ansible were absent

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| done   | bug  | P1       | S    | —          |

## Symptom

In the editor, with ansible-core 2.21.3 detected (`ansible-lsp detect: path-walk-up (core
2.21.3)` in the log, status bar "found"), every module outside the workspace hovered the same
line. Measured over LSP against the release binary, waiting for the detect log first:

| Token                    | Hover                                                               |
| ------------------------ | ------------------------------------------------------------------- |
| `ansible.builtin.debug`  | **Skipped** — not in this workspace (a builtin, or installed outside it) |
| `ansible.builtin.copy`   | same                                                                |
| `community.general.ufw`  | same, though the collection ships in the detected package           |

Go-to-definition on those went nowhere. The same install answered correctly from `scan` and
from every test that builds its own `FileContext`, which is why nothing flagged it.

Every consumer of `State::install()` was affected: reference hover and definition, the
workspace scan's `ScanCache`, injected-variable hover, and the version gate in
`diagnostics_with`.

## Cause

`867a217` (2026-08-22) moved the install from a process-wide `OnceLock` onto
`State::install`. `startup` kept the detection, the log line and the `AnsibleStatus`
notification, but the store was lost in the move: the slot was built as `Mutex::new(None)` and
nothing wrote to it.

It survived three weeks because the handler tests never ran `startup` with an install, and
`hover_shows_module_provenance_not_paths` (T-029 box 4's pin) discovered its context without
one, then returned early when the builtin failed to resolve. Turned into a panic, that return
fired: the test had never reached an assertion on a machine with Ansible installed.

## Fix

`startup` stores the detected install on `state` before notifying the client.

## Done when

- [x] a handler-level test runs `initialize` → `initialized` → startup with a fake install
      handed over through `ansiblePath`, and asserts the stored install, a hover reading
      "from the Ansible install", and a definition into that install — with a control hover
      before startup. `startup_stores_the_detected_install_for_hover_and_definition`, seen red
      with the store removed (`left: None`)
- [x] `hover_shows_module_provenance_not_paths` attaches the install it detects and asserts the
      builtin resolves instead of returning; seen red by breaking the origin string. Its
      no-Ansible early return is T-203's
- [x] measured over LSP on the rebuilt binary: `ansible.builtin.debug` and `copy` hover
      "from the Ansible install", `community.general.ufw` "from an installed collection", and
      definition on `copy` opens the installed `modules/copy.py`
