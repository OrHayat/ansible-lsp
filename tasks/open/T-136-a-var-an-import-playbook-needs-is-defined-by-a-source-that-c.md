# T-136 — A var an import_playbook needs is defined by a source that cannot reach it

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P1       | M    | T-099 | T-095      |

## Problem

```yaml
# group_vars/all.yml
env: prod

# site.yml
- import_playbook: "{{ env }}-setup.yml"
```

This never works. `group_vars` cannot supply a value to an `import_playbook`, so the author's
definition is **inert** — it looks like it configures the import and does nothing. The whole
playbook dies at parse time with `'env' is undefined`, and the editor, which has both halves
of this in its index, says nothing about the connection.

Split out of T-095. T-095 stops the wrong message; this ticket adds the one thing neither
Ansible nor ansible-lint can say — *which definition the author is relying on, and why it is
inert*.

## Why it happens

`import_playbook` is templated when the playbook file is **parsed**, by
`variable_manager.get_vars()` called with no play, host or task
(`playbook_include.py:69-83`). Every source that needs one of those three is out of scope.
T-095 carries the full table and the live run against ansible-core 2.21.2 confirming it;
short version: only `-e` and a literal `vars:` on the import entry work.

Now line that up against `VarSource` (`vars.rs:31-57`), everything the index knows:

| `VarSource` | Needs | Can reach a parse-time import? |
| ----------- | ----- | ------------------------------ |
| `PlayVars`, `BlockVars`, `TaskVars` | a play / block / task | no |
| `SetFact`, `Register` | a host, at runtime | no |
| `VarsFiles` | a play | no |
| `RoleDefaults`, `RoleVars` | a play | no |
| `GroupVarsAll`, `GroupVars`, `HostVars` | a host | no |
| `IncludeVars` | a task | no |

**All twelve are unreachable.** Extra-vars is deliberately not indexed (`vars.rs:60-61`
— precedence 22, outside what a static index can see), and a `vars:` on the import entry
is not indexed at all today. So the rule needs no per-source classification: *if the index
resolves the name, that definition cannot be the one supplying it.*

## What this must NOT claim

**Not "the playbook is broken".** A variable in `group_vars` can also be passed with
`-e env=prod` at launch — group_vars as the default, `-e` as the override, is an ordinary
pattern. The command line is invisible to us, so **no case here is provably fatal**, and an
earlier draft of T-095 that said otherwise was wrong.

What *is* provable is T-095's half: the file cannot pass `ansible-playbook --syntax-check`
(exit 4, live-verified), because that command takes no user arguments. This ticket's addition
sits on top of that and is also certain, being a statement about the definition rather than
about the run:

> `env` is defined in `group_vars/all.yml:1`. That cannot supply an `import_playbook`, which
> is expanded before any host exists — only `-e env=…` or a `vars:` on this line can.

That is true regardless of the command line. It names the file the author actually wrote,
which is the part they will otherwise stare at for an hour.

Severity: **warning**, not error. It fires on a real misconception, but it cannot know the
launch command. If the corpus shows it is noisy, downgrade to a hint rather than widening it.

## Approach

1. On a templated `ImportPlaybook` reference, pull the variable names out of the template
   (`references.rs` already extracts uses for T-049).
2. Ask the var index for each name. A hit → emit, naming the defining file, line and
   `VarSource`. A miss → say nothing; that is T-095's "requires `-e`" case.
3. The `vars:` on the import entry has to be read first, or it is a false positive on the
   one spelling that *does* work — T-095 covers reading it, hence the dependency.

## Watch out

- Deriving "unreachable" from the `VarSource` list means a **new** variant added later is
  silently assumed unreachable. Make that a match with no wildcard arm, so adding a source
  fails the build until someone decides which side it is on.
- `demo/` needs the fixture: `group_vars/all.yml` defining `env`, a templated import using
  it, and the `-e` spelling next to it as the GOOD case.
- The rule needs the corpus run before it ships — `~/app/ansible` has 48 conditional imports;
  if a meaningful number are templated and this fires on all of them, the framing is wrong.

## Done when

- [ ] a templated `import_playbook` whose var the index defines names that file and source
- [ ] the message says the definition is inert, never that the playbook fails
- [ ] a `vars:` on the import entry suppresses it entirely
- [ ] a var the index does not know stays silent here (T-095 owns that case)
- [ ] adding a `VarSource` variant fails the build rather than defaulting to unreachable
- [ ] the corpus scan is run and the hit count recorded here
