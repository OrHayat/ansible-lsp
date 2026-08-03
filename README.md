# ansible-lsp

Go-to-definition, broken-reference diagnostics, and `when:` explanations for Ansible, as a
Rust language server. VS Code today via a thin client; Neovim later via lspconfig against the
same binary.

Replaces `community-local.ansible-role-goto`, which resolves paths relative to the including
file — wrong in the four real cases pinned in `resolve.rs`'s tests.

## Layout

```
crates/ansible-core/   all logic, no LSP, no bindings — where the tests live
  parse.rs             byte-span AST types + byte<->line/UTF-16 index
  parse_libyaml.rs     the YAML parser (libyaml, lenient like Ansible). Only parser module.
  references.rs        AST -> the cross-file references we care about
  resolve.rs           reference -> file on disk, in Ansible's search order
  workspace.rs         role dir / tasks dir / project root / search roots
  config.rs            ansible.cfg: roles_path, collections_path
  install.rs           the installed ansible, so builtins and collections resolve
  glob.rs              templated "{{ x }}/y.yml" -> every file it could reach
  bin/scan.rs          resolve a whole tree; non-zero exit on missing files
crates/ansible-lsp/    thin tower-lsp shim over the core
client/                ~140-line VS Code extension (plain JS, no build step)
scripts/smoke.js       drives the server over raw stdio, no editor needed
```

## Build and run

```sh
cargo build --release
cargo test                      # incl. real-repo regressions
./target/release/scan ~/app/ansible   # whole-repo report / CI check
node scripts/smoke.js           # end-to-end over LSP against ~/app/ansible
python3 scripts/inlay-hints.py  # `when:` hover per settings combination, no editor
```

`scripts/smoke.js` needs node on PATH — it's installed via nvm but not exported in
non-interactive shells:

```sh
export PATH=~/.nvm/versions/node/v24.11.0/bin:$PATH
```

## Try it in VS Code

Open this folder and run **Run → Start Debugging** (F5 where the key is free — on some Macs
it's the dictation key and does nothing). That launches an Extension Development Host with the
`demo/` folder open. Ctrl+click (Cmd+click on macOS) any `include_tasks:` value, and hover a
`when:` on an `import_playbook` to see what the condition does.

On Windows the server binary is `ansible-lsp.exe`; the client resolves that automatically.
Rebuilding the release binary needs the Dev Host stopped first — Windows locks a running
`.exe`.

Set `ansibleLsp.trace.server` to `verbose` to watch LSP traffic in the *Ansible LSP*
output channel.

## What it resolves

| Reference | Resolves to |
| --------- | ----------- |
| `include_tasks` / `import_tasks` | role `tasks/` -> role dir -> file dir -> project root |
| `include_role` / `import_role`, `roles:` | `roles_path` from ansible.cfg, then collections |
| `tasks_from:` | that role's `tasks/<name>.yml`, block and flow forms |
| module FQCN | in-repo collections, installed collections, and ansible.builtin |
| templated `{{ }}` paths | every file the pattern could reach (never warns) |

Resolvable references are coloured; literal paths that resolve to nothing get a warning
listing every path tried. The whole workspace is scanned at startup, so a broken
reference shows up even in files you never opened.

`demo/tasks/main.yml` exercises all of it, labelled good/bad.

## Deliberate silences

These never warn, each for a reason a test pins down:

- **templated paths** — the target depends on runtime variables, so absence proves nothing
- **a role with no `tasks/main.yml` but a `tasks_from`** — legal; `roles/cib-batch` in the
  real repo is exactly this, and 16 working references depend on it
- **modules from collections that aren't installed** — an uninstalled dependency, not a typo

(A file that fails to parse is *not* in this list anymore: the parser matches Ansible's, so a
parse failure is a real one and gets an `unparseable` error — see the finding below.)

## Status

The backlog is the single source of truth: [`tasks/README.md`](tasks/README.md). Open
tickets, what's shipped, and what was rejected all live there, so this section can't drift.

## Two findings that shape the code

**Match Ansible's parser, not the spec.** Strict YAML 1.2 rejects real playbooks Ansible
runs — e.g. `roles/lustre-nvme-binding/tasks/_run.yml:45`, a multi-line double-quoted scalar
whose continuation lines aren't indented past their key. The old strict parser (saphyr, and
yaml-rust2) errored on it; PyYAML/libyaml don't enforce that rule, so it ships in production.
We parse with `libyaml-safer` (a pure-Rust libyaml port), which accepts exactly what Ansible
accepts (T-036, verified: 729/729 corpus). Consequence: a file we can't parse is one Ansible
can't load either, so it's a real `unparseable` **error**, not a silent gap.

**Byte offsets, not character offsets — but watch the third coordinate.** Three coordinate
systems are in play: source bytes, char columns, and LSP UTF-16. libyaml's marks are byte
offsets, so `parse_libyaml.rs` builds byte spans directly — but LSP still wants UTF-16, and
any line with non-ASCII (this repo has em dashes and emoji in names) will shift ranges if the
two are mixed. The em-dash / emoji round-trip tests pin it.
