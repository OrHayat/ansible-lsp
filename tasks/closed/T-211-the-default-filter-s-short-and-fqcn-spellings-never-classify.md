# T-211 — The default filter's short and FQCN spellings never classify

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | bug  | P2       | S    | T-114 | —          |

## Symptom

The same condition written two legal ways gets two different answers. Measured by calling
`classify` directly:

```
skip_setup | default(false)          -> OnlyIfSet       "runs only if skip_setup is set"
skip_setup | d(false)                -> Unknown         no label
skip_setup | default(true)           -> UnlessCleared   "runs unless skip_setup is false"
skip_setup | d(true)                 -> Unknown         no label
x | ansible.builtin.default(false)   -> Unknown         no label
```

All three spellings are the same filter. In jinja2 3.1.6, `filters.py:1825` registers `"d"`
and `"default"` against one function object — `FILTERS['d'] is FILTERS['default']` is `True`.
All three run correctly on ansible-core 2.21.3, verified with controls that must differ:
`| d(false)` and `| ansible.builtin.default(false)` skip the task, the `(true)` forms run it.

This is the shape that drives the "runs unless…" guard hover, so where a repo prefers the
short spelling the hover is not merely degraded — it is absent.

## Cause

`parse_defaulted` matches the *literal text* `default(` (`condition.rs:1166`):

```rust
if let Some(arg) = p.strip_prefix("default(").and_then(|a| a.strip_suffix(')')) {
    dflt = Some(arg.trim().to_string());
} else if p != "bool" {
    // An unrecognised filter could change the result; don't guess.
    return None;
}
```

`d(false)` is not that literal and is not `bool`, so it takes the `return None` branch. That
branch is right — an unrecognised filter really could change the result — it is just firing
on a filter that is not unrecognised. Jinja's `parse_filter` also accumulates dotted filter
names (`name += "." + expect("name")`), which is why the FQCN spelling parses upstream and
misses here for the same reason.

## Fix

Compare the filter's *name* against a known set rather than prefix-matching the text: accept
`d` and `default`, and strip a leading `ansible.builtin.` / `ansible.legacy.`. The `return
None` guard for genuinely unknown filters must stay exactly as strict — widening it is how
this rule would start guessing.

T-188 dissolves this rather than fixing it: with a `Filter { name, args }` node the check is
a comparison on `name`, not a string pattern. Filed separately for the same reason T-187 is —
it is independently shippable, and T-188 is an unstarted L.

It also gives T-188 something its box 7 currently lacks: a pre-tree baseline to measure
against. Note the consequence honestly when that lands — fixing the spelling here moves the
number *before* the tree, so the tree will read as buying correctness rather than reach.

## Measured spread

Distinct non-test clauses across five public trees, parsed with Jinja's own parser:

| tree | commit | `d(` | `default(` | FQCN |
| ----------------------------------------- | --------- | ---: | ---: | ---: |
| `debops/debops` | `65b66ff` | 852 | 1 | 0 |
| `kubernetes-sigs/kubespray` | `46dbdd3` | 1 | 59 | 0 |
| `openstack/openstack-ansible` | `b83dc69` | 0 | 4 | 0 |
| `ansible/ansible` | `b85437b` | 0 | 0 | 0 |
| `ansible-collections/community.general` | `0bf15b1` | 0 | 0 | 0 |

The raw totals (853 vs 64) overstate the case and should not be quoted on their own: the
split is **per-repo house style**, not a global preference — one tree uses `d` almost
exclusively, another uses `default` almost exclusively. That is the actual argument for
fixing it. The failure is not "silent on 56% of lines everywhere", it is "silent on nearly
every guarded condition in some codebases, and on none in others".

FQCN has **zero** occurrences anywhere and is included only because it is the same change.

## Done when

- [x] `x | d(false)` classifies identically to `x | default(false)`, asserted on both
      `label()` and `requirement()`
- [x] the same for a second arm, so the fix is not pinned to one verdict: `x | d(true)` gives
      `UnlessCleared`
- [x] `x | ansible.builtin.default(false)` classifies identically to the bare spelling
- [x] an unrecognised filter still returns `None` — a test naming one that must stay refused,
      so the `don't guess` guard is proved not to have widened
- [x] seen red before the fix, and the break verified to be in the file (rule 5)


## Outcome

Fixed by [[T-188]]'s rewrite rather than as a change of its own: once a filter is a node with
a name, `d`, `default` and both FQCN spellings are the same shape, and `strip_guards` matches
on `name.rsplit('.').next()`.

**The measured spread held up.** This ticket predicted debops would carry the gain because it
uses `d(` 852 times where other trees use `default(`. Over the eight pinned trees, the rewrite
classified **+156** conditions and **71 of them are the `d(` alias** — debops alone went
204 → 355, while five of the eight trees moved by 5 or fewer. The per-repo framing this ticket
argued for was the right one: the blended `+10%` would have hidden a `+74%` and three zeros.

Guard against widening: `dd(`, `default_if_none(` and `frobnicate(` are asserted to stay
`Unknown`. Seen red — treating any filter beginning `d` as `default` makes `skip_x | dd(false)`
classify, and the test fails on it.
