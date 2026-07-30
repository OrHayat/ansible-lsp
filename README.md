# ansible-lsp

Go-to-definition and (soon) broken-reference diagnostics for Ansible, as a Rust language
server. VS Code today via a thin client; Neovim later via lspconfig against the same binary.

Replaces `community-local.ansible-role-goto`, which resolves paths relative to the including
file — wrong in the four real cases pinned in `resolve.rs`'s tests.

## Layout

```
crates/ansible-core/   all logic, no LSP, no bindings — where the tests live
  parse.rs             YAML -> byte-span AST. The only module touching saphyr.
  references.rs        AST -> the cross-file references we care about
  resolve.rs           reference -> file on disk, in Ansible's search order
  workspace.rs         role dir / tasks dir / project root / search roots
  config.rs            ansible.cfg: roles_path, collections_path
  install.rs           the installed ansible, so builtins and collections resolve
  glob.rs              templated "{{ x }}/y.yml" -> every file it could reach
  bin/scan.rs          resolve a whole tree; non-zero exit on missing files
crates/ansible-lsp/    thin tower-lsp shim over the core
client/                ~50-line VS Code extension (plain JS, no build step)
scripts/smoke.js       drives the server over raw stdio, no editor needed
```

## Build and run

```sh
cargo build --release
cargo test                      # 37 tests, incl. real-repo regressions
./target/release/scan ~/app/ansible   # whole-repo report / CI check
node scripts/smoke.js           # end-to-end over LSP against ~/app/ansible
python3 scripts/inlay-hints.py  # inlay hints per settings combination, no editor
```

`scripts/smoke.js` needs node on PATH — it's installed via nvm but not exported in
non-interactive shells:

```sh
export PATH=~/.nvm/versions/node/v24.11.0/bin:$PATH
```

## Try it in VS Code

Open this folder and press **F5**. That launches an Extension Development Host with
`~/app/ansible` open. Cmd+click any `include_tasks:` value.

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
- **files that fail to parse** — strict YAML 1.2 is stricter than Ansible's PyYAML

## Status

Steps 0–4 of the plan (`~/.claude/plans/fancy-stirring-aurora.md`). Remaining: the
differential harness against the legacy plugin, the execution tree (`callHierarchy`),
and the Neovim lspconfig entry.

## Two findings that shape the code

**saphyr markers are character offsets, not byte offsets.** Invisible in ASCII; wrong on
any line containing non-ASCII, which this repo has (em dashes in task names). `parse.rs`
converts once, at the boundary, so everything downstream can assume bytes. There are three
coordinate systems in play — saphyr chars, Rust bytes, LSP UTF-16 — and mixing them silently
shifts ranges.

**Strict YAML 1.2 is stricter than Ansible.** `roles/lustre-nvme-binding/tasks/_run.yml:45`
fails in both saphyr and yaml-rust2 but PyYAML accepts it, so it runs in production. An
unparseable file must therefore yield no references and no diagnostics — never a false
"missing file" warning. "Does it parse" is not a proxy for "is it valid Ansible."
