# T-163 — Row 8's remaining cases: judging a file as a playbook when nothing in it says so

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | task | P3       | M    | T-020      |

## Problem

T-110 row 8 is four faults from one `if/elif` chain (`playbook/__init__.py:74-91`). One shipped;
three did not:

| case | ansible says | why it is not shipped |
| ---- | ------------ | --------------------- |
| an entry is not a dict | `playbook entries must be either valid plays or 'import_playbook' statements` | **shipped** — a sibling entry is a real play, which proves the file's kind |
| the file is empty | `Empty playbook, nothing to do: <path>` | no play inside, so nothing proves it is a playbook |
| top level is not a list | `A playbook must be a list of plays, got a <class ...> instead` | same |
| top level is `[]` | `A playbook must contain at least one play` | same |

The three are exactly the cases where the file is too empty, or too wrong, to contain the
evidence that it is a playbook. What makes it an error is what removes the proof. So the
evidence has to come from outside the file.

## Approach

### Most of this is already decidable, and the residual is small

The ambiguity is narrower than "any YAML file". Ruling out by path and by name covers nearly
everything:

- anything under `group_vars/`, `host_vars/`, `defaults/`, `vars/`, `meta/` is a known kind and
  never a playbook — T-150's matrix
- a root-level file with a reserved name is a known kind: `galaxy.yml`, `requirements.yml`,
  `ansible.cfg`, an inventory file named in `ansible.cfg`
- anything reached by `import_playbook:` **is** a playbook, and needs none of this — that slice
  ships separately against `include_target.rs`, since the reference proves the kind the same
  way `import_tasks:` does for a task file

What is left after all that is one shape: an **unreferenced, unknown-named YAML file** that is
empty, or a bare mapping, or `[]`. In a real repo that is usually a scratch file, a placeholder,
or a playbook someone started and has not written yet — and only the last deserves a
diagnostic.

### Why T-020 is the dependency and not T-150

T-150 answers "what kind is this file, by path?" and gets us the exclusions above. It cannot
answer the last question, because the answer is not in the file or its location:

> is this file referenced by anything, and as what?

A file that some play lists in `vars_files:` is a vars file whatever it is named. A file nothing
references at all is a plausible entry point — the thing you type after `ansible-playbook`. That
is a reverse-index query, which is T-020.

**The decision rule to implement**, once the index exists:

1. known kind by path or reserved name → not a playbook, stay silent
2. referenced as a vars file / task file / inventory → that kind, stay silent
3. referenced by `import_playbook:` → a playbook, apply all three rules (but that path is
   already covered, see above)
4. referenced by nothing → a candidate entry point, apply all three rules

Step 4 is the one that needs deciding rather than deriving: an unreferenced empty YAML file is
*probably* an unfinished playbook, and "probably" is a warning, not an error — even though
ansible's own message is fatal. Worth settling against how noisy it turns out to be on a real
repo before picking the tier.

## Done when

- [ ] an empty file, a top-level mapping, and `[]` each produce their upstream message when the
      file is an entry point by the rule above
- [ ] the exclusions are asserted, not assumed: a `group_vars/` file, a `vars_files:` target and
      a `requirements.yml` each stay silent, one test per kind
- [ ] the tier for case 4 is settled by counting false positives on a real repo, and the count
      is recorded here — the rule is only worth having if that number is near zero
- [ ] T-110 row 8's box points here, and the three cases are struck there rather than
      re-litigated
