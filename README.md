# ansible-lsp

Go-to-definition and (soon) broken-reference diagnostics for Ansible, as a Rust language
server. VS Code today via a thin client; Neovim later via lspconfig against the same binary.

Replaces `volumez-local.ansible-role-goto`, which resolves paths relative to the including
file — wrong in the four real cases pinned in `resolve.rs`'s tests.

## Layout

```
crates/ansible-core/   all logic, no LSP, no bindings — where the tests live
  parse.rs             YAML -> byte-span AST. The only module touching saphyr.
  references.rs        AST -> the cross-file references we care about
  resolve.rs           reference -> file on disk, in Ansible's search order
  workspace.rs         role dir / role tasks dir / project root discovery
crates/ansible-lsp/    thin tower-lsp shim over the core
client/                ~50-line VS Code extension (plain JS, no build step)
scripts/smoke.js       drives the server over raw stdio, no editor needed
```

## Build and run

```sh
cargo build --release
cargo test                      # 14 tests, incl. 4 real-repo regressions
node scripts/smoke.js           # end-to-end over LSP against ~/matrix/ansible
```

`scripts/smoke.js` needs node on PATH — it's installed via nvm but not exported in
non-interactive shells:

```sh
export PATH=~/.nvm/versions/node/v24.11.0/bin:$PATH
```

## Try it in VS Code

Open this folder and press **F5**. That launches an Extension Development Host with
`~/matrix/ansible` open. Cmd+click any `include_tasks:` value.

Set `ansibleLsp.trace.server` to `verbose` to watch LSP traffic in the *Ansible LSP*
output channel.

## Status

Step 1 of the plan (`~/.claude/plans/fancy-stirring-aurora.md`): `include_tasks` /
`import_tasks` only, definition only. Remaining reference kinds, diagnostics, and the
execution tree are steps 3–5.

## Two findings that shape the code

**saphyr markers are character offsets, not byte offsets.** Invisible in ASCII; wrong on
any line containing non-ASCII, which this repo has (em dashes in task names). `parse.rs`
converts once, at the boundary, so everything downstream can assume bytes. There are three
coordinate systems in play — saphyr chars, Rust bytes, LSP UTF-16 — and mixing them silently
shifts ranges.

**Strict YAML 1.2 is stricter than Ansible.** `roles/daos-nvme-binding/tasks/_run.yml:45`
fails in both saphyr and yaml-rust2 but PyYAML accepts it, so it runs in production. An
unparseable file must therefore yield no references and no diagnostics — never a false
"missing file" warning. "Does it parse" is not a proxy for "is it valid Ansible."
