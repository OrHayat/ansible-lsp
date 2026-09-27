# T-109 — Keyword value enums

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P2       | M    | T-106 | T-107      |

## Problem

A handful of keywords take a value from a closed set, and the enforcement is inconsistent
about *when* it fires — which is what makes two of them worth diagnosing.

| Keyword    | Legal values | Enforced |
| ---------- | ------------ | -------- |
| `debugger` | `always on_failed on_unreachable on_skipped never` | **run** time, per task (`base.py:206-209`) — see Landed |
| `order`    | `inventory sorted reverse_sorted reverse_inventory shuffle` | **run** time (`inventory/manager.py:438-439`) |
| `strategy` | any loaded strategy plugin | **run** time (`task_queue_manager.py:380`) |
| `connection` | any loaded connection plugin | **run** time — `connection: locl` → "the connection plugin 'locl' was not found" |
| `become_method` | any loaded become plugin | **run** time, and only once `become` is in effect — `become_method: sude` with `become: true` → "Invalid become method specified, could not find matching plugin: 'sude'"; without `become`, `ok=1` |
| `serial`   | int, percent string, or list thereof | `<= 0` silently means "all hosts" (`playbook_executor.py:286-287`) |

`order:`, `strategy:`, `connection:` and `become_method:` survive `ansible-playbook
--syntax-check` entirely — the run-time check is reached only when the play actually starts —
so a typo there can pass CI and fail in production. Measured 2.21.3: `strategy: linaer` passes
`--syntax-check` and fails the run with "Invalid play strategy specified: linaer";
`connection: locl` the same with the message in the table. Those are the reason this is a
ticket and not a footnote.

`serial: 0` is not an error at all; it just means everything at once, which is the opposite
of what someone writing `serial:` usually wants.

## Approach

Enum sets are one more column on the T-107 tables. `strategy`, `connection` and `become_method`
need the plugin index rather than a literal list — core's shipped set plus T-227's per-type dirs
and the collection `plugins/<type>/` trees, since a collection can add any of the three — so
they should degrade to no-diagnostic when Ansible is not installed, the same concession the
module rules already make.

## Done when

- [x] `debugger:` and `order:` outside their sets are ERRORs
- [ ] an unknown `strategy:` warns when the plugin index is available, and is silent otherwise
- [ ] an unknown `connection:` warns under the same gate
- [ ] an unknown `become_method:` warns, worded "fails once become is in effect" — it is inert
      without `become`, and `become` can arrive from cfg, `-b` or inventory, so the name check
      does not wait for it
- [x] `serial: 0` gets a HINT saying it means all hosts
- [ ] the enums live with the keyword tables, not in a second place

## Progress

| Keyword          | State        | Where                                                         |
| ---------------- | ------------ | ------------------------------------------------------------- |
| `debugger:`      | **done**     | ERROR, `keyword_values.rs`                                    |
| `order:`         | **done**     | ERROR, `keyword_values.rs`                                    |
| `serial:`        | **done**     | HINT, `keyword_values.rs`                                     |
| `strategy:`      | not started  | needs the plugin index                                        |
| `connection:`    | not started  | needs the plugin index                                        |
| `become_method:` | not started  | needs the plugin index                                        |

**What is left is the larger half.** The three open keywords have no fixed value set — the
legal values are whatever plugins of that type are installed, and a collection can add any
of them — so each needs a per-type plugin index (core's shipped set, T-227's per-type dirs,
collection `plugins/<type>/`) before the check can be written. That is M, not S. The last
done-when box stays open until those enums exist too.

Tests: `keyword_values::tests` (10 — task file, duplicate keys, every legal value, case,
templated, per-level wording, `serial` boundaries and messages, keys off their node) and
`the_keyword_values_demo_matches_its_annotations_exactly` /
`every_other_demo_file_is_free_of_keyword_value_diagnostics` over `demo/keyword_values.yml`.
Each was seen red with the code broken.

## Measured on 2.21.3

This corrected the table in Problem. `debugger:` is **not** a load-time fatal:
`--syntax-check` passes and each task it applies to fails when it runs ("Error processing
keyword 'debugger'"). Under `when: false` the task skips clean; `ignore_errors: true`
swallows it. Still ERROR — the value is broken on every run that reaches it — worded
"fails whenever it runs". Both enums are case-sensitive (`Always`, `Sorted` fail) and both
template, so `{{ }}` values are skipped.

`serial:` on 3 hosts, counting batches: `0`, `"0"`, `-1`, `[0]` → 1 (no batching);
`"0%"`, `"-10%"`, `["0%"]` → 3 (one host at a time — a percentage never yields a batch
under one); `[1, 0]` → 2 (the `0` takes all remaining). All of those hint. `"10%"` also gives
3 here, but that depends on inventory size, so it does not. `serial: "0.0"` crashes Ansible
with an unhandled `int()` error — not handled here.
