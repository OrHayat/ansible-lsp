# T-218 — Role with no tasks/main.yml hovers as "not in this workspace"

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | bug  | P1       | S    | T-090 | —          |

## Symptom

Hovering the role name in an `include_role` whose role has no `tasks/main.yml` says the role
is somewhere it is not:

```
**Skipped** — not in this workspace (a builtin, or installed outside it)
```

The role *is* in the workspace. This shape is legal Ansible and works:

```yaml
- include_role:
    name: batch-window
    tasks_from: begin        # loads roles/batch-window/tasks/begin.yml
```
```
roles/batch-window/tasks/begin.yml     exists
roles/batch-window/tasks/commit.yml    exists
roles/batch-window/tasks/main.yml      absent, and not needed
```

Measured, not read — `reference_hover` run against a fixture of exactly that shape:

```
ROLE "batch-window"  status=Skipped  skip=Some(NotInWorkspace)
   HOVER: "**Skipped** — not in this workspace (a builtin, or installed outside it)"
ROLE "totally-absent" status=Missing  skip=None
   HOVER: None
```

The second row is the control: a role that really is absent takes a different path, so the
probe could have come out otherwise.

`scan` inherits the same mislabel and files these under `UNRESOLVED ROLE NAMES`, beneath the
`MISSING FILES` block. Reported from a 779-file private tree: 18 such call sites (all one
role) against 3 genuine missing-file hits — a 6:1 ratio of correct-and-uninteresting rows
above the real ones, under a heading that reads as a problem list.

**Not in scope: the verdict.** Resolution is right and stays right. `Status::Skipped` for the
role and `Resolved` for the `tasks_from` is what `resolve.rs`'s
`role_without_main_is_fine_when_tasks_from_is_given` asserts, and its comment records 16
working references that depended on it not warning. Measured alongside: a bogus `tasks_from`
and a role that does not exist at all are both still reported as missing. This ticket is only
about the sentence.

## Cause

`SkipReason` has three variants and none of them means "the role directory is here, it just
has no `main.yml` because every caller passes `tasks_from`". That case is classified
`NotInWorkspace` at `resolve.rs:778` — whose own doc comment reads *"Nothing to point at in
this workspace (e.g. `ansible.builtin.*`, or a role installed outside it)"*, which is false
here.

Every consumer then inherits the wrong claim, because the distinction was never put on the
data. Read sites enumerated rather than assumed:

| consumer          | today                                                    |
| ----------------- | -------------------------------------------------------- |
| `main.rs:2950`    | prints the "not in this workspace" sentence — user-visible |
| `scan.rs:217`     | collects it into `UNRESOLVED ROLE NAMES`                   |
| `vars.rs:1312`    | tests only `Templated` — unaffected                        |
| `resolve.rs:778`  | the classification site                                    |

## Fix

Rule 3 — the distinction belongs on the data, not in one caller. Add a fourth `SkipReason`
for "role found, no `tasks/main.yml`, reached via `tasks_from`" and set it at the
classification site when the role directory resolves, `tasks/main.yml` does not, and the
reference carries `has_tasks_from`. Adding a variant makes the `main.rs` match arm a compile
error rather than letting it silently keep the old sentence.

Then each consumer answers from it: a hover that says the role is here and which file the
include actually loads, and its own `scan` section instead of `UNRESOLVED ROLE NAMES`.

## Done when

- [x] A fourth `SkipReason` distinguishes this case, set at the single classification site
- [x] The hover names the role as present and does not say "not in this workspace"
- [x] `scan` no longer lists these under `UNRESOLVED ROLE NAMES`
- [x] A test per consumer that reads `skip_reason`, not one test for the rule (rule 3)
- [x] Each new test seen failing before the fix, for the right reason (rule 5)
- [x] The verdict is unchanged: role `Skipped`, `tasks_from` `Resolved`, and a bogus
      `tasks_from` or an absent role still reported missing

## Outcome

The section was not merely excluded from `UNRESOLVED ROLE NAMES` — after the fix a `Role`
reference can no longer reach `NotInWorkspace` by any path, so that collector was dead code.
It is repointed at the new variant under a heading that states the case is expected, which
keeps `every_finding_section_prints_when_the_tree_earns_it` meaningful.

**This ticket fixed the lie, not the silence, and the fix is provisional — T-063 owns the
rest.** Measured after the fix: the Role reference still resolves to nothing, so
go-to-definition on the role name does nothing whenever `tasks/main.yml` is absent, while the
`tasks_from` token on the same line jumps correctly.

The reason that is T-063's and not a reopen here: `main` is only the *default* for
`tasks_from`, and `vars_from`/`defaults_from`/`handlers_from` move the entry the same way for
their own subdirs — Ansible decides all four in `_load_role_yaml`. So the real fix is to
resolve the entry as `tasks/<tasks_from or main>`, at which point this case resolves outright,
`SkipReason::RoleWithoutMainTasks` becomes unreachable, and `scan`'s no-main section goes with
it. T-063 carries a done-when box for exactly that. Closing this one is not a claim that roles
without `tasks/main.yml` are fully handled.
