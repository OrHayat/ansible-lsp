# T-019 — Package as a .vsix

| Status | Priority | Size | Epic  | Depends on |
| ------ | -------- | ---- | ----- | ---------- |
| open   | P2       | M    | T-130 | —          |

## Problem

The server only exists while an Extension Development Host window is open. Every real use
means: open this repo in VS Code, Run -> Start Debugging, work in the child window. Close it
and the tool is gone.

That's fine for building it and useless for using it. `redhat.ansible` and
`community-local.ansible-role-goto` are both disabled in favour of this, so right now normal
Ansible work in a normal window has *less* navigation than before the project started.

## Approach

`vsce package` -> `ansible-lsp-0.1.0.vsix` -> `code --install-extension`. Local file, no
marketplace, no publisher account — scope is still "just me."

What has to be decided, because it's the actual work:

- **Where the binary lives.** The client currently points at a `target/release` path in this
  checkout. A packaged extension can either bundle the binary inside the `.vsix` (self-contained,
  but `vsce package` has to run after `cargo build --release`, and it's macOS-arm64-only) or
  keep reading an absolute path from a setting (stays in sync with rebuilds, breaks if the
  checkout moves). Bundling is the right default; a setting to override it costs two lines and
  covers development.
- **Activation stays `onLanguage:ansible` / `onLanguage:yaml`.** An installed extension is
  loaded in every window, so this is now load-bearing rather than tidy: opening VS Code on a
  non-YAML file must not start the server.
- **A rebuild no longer picks itself up.** With the dev host, rebuilding and relaunching was
  the loop. Installed, it needs a reinstall — worth a one-line `make install` so the loop
  doesn't become "why is my fix not there."

Sequencing note: this is worth doing *after* T-012, because a stale-diagnostics bug is much
more annoying in a long-lived window than in a debug session you restart constantly.

## Done when

- [ ] `.vsix` installs and works in a normal VS Code window, no debug host
- [ ] the binary is found without depending on the checkout's path
- [ ] activation still doesn't fire on non-YAML files
- [ ] one documented command rebuilds and reinstalls
