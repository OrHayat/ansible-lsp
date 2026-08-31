# T-216 — A stray {% inside a raw body is read as a tag, so a template that renders is flagged unterminated

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| done   | bug  | P1       | S    | —          |

## Symptom

A `.j2` that renders fine gets a `template-syntax` ERROR reading **Missing end of raw
directive**, on a `{% raw %}` block that is plainly closed. Measured through `did_open`, so
this is the squiggle a user sees and not just a reader's return value.

Two real templates in `~/app/ansible` hit it — both wrap a script body in `{% raw %}` and
print a format string inside it:

```
{% raw %}
    awk '{ printf "cgroup_io_rbytes_total{%s} %.0f\n", l, s["rbytes"] }'
{% endraw %}
```

Reduced, and the quoting is not needed — this is the whole bug:

```
{% raw %}
plain {%s} text
{% endraw %}
```

jinja2 3.1.6 lexes that as `raw_begin`, one `data` run, `raw_end`. We refuse it.

**None of the eight public corpus trees contain a `{%` inside a raw body**, so 1565 templates
never produced it and 125 did. The two corpora catch different things and neither subsumes
the other — [[T-040]]'s gate should be run against both.

## Cause

`scan_raw` (`template.rs`) looked for the next `{%`, then called `find_end` to locate its
`%}`. `find_end` is the **tag** scanner — bracket depth and string tracking — and inside a raw
body there is no tag grammar at all. On `plain {%s} text{% endraw %}` it scans from after the
stray `{%`: the `}` of `{%s}` takes the depth to -1 (clamped to 0), then the `{` of the *real*
`{% endraw %}` takes it to 1, so that tag's own `%}` is never seen at depth zero. `find_end`
returns `None` and the arm handling that refused the raw on the spot.

Measured rather than read off the code: the two error arms were given distinct messages under
the pre-fix source, and it is the `find_end`-found-nothing arm that fires — not the
loop-ran-out-of-`{%` arm an earlier draft of this section named.

Upstream's `raw_end` rule is a literal match for the endraw tag, never a parse — everything
until one is data.

## Fix

`raw_end` matches upstream's rule directly: block start, optional `-`/`+`, `endraw`, block end
with optional `-`. A `{%` that is not an endraw advances by the delimiter's own length instead
of past a computed tag end. It returns the offset of the closing delimiter, so both trim
markers are read off the same offsets a parsed tag gave them.

Seen red on all three surfaces by restoring the old skip: the lexer test, the `did_open` test,
and the corpus gate back to `falsely-refused=2`.

## The pinned trees, re-measured

The eight public trees were not on this machine when the last box was ticked. Cloned at their
pinned revisions and re-run — seven at the exact SHAs T-184 records, `ansible/ansible` at
`8ebd2d6ee8` rather than `b85437b`, so its file counts run higher:

```
block-split   templates=1614 refused=5                                       differed=0
references    templates=1603 literal=57 dynamic=1 headers=6 falsely-refused=0 differed=0
```

Every column that is not a file count matches what the gate recorded before — `literal=57`,
`dynamic=1`, `headers=6`, `refused=5`, and both zeros — so the tree revision moves the count
and nothing else.

The Symptom's "no public tree contains this shape" is measured now rather than assumed: walking
`raw_begin`/`data`/`raw_end` with jinja2's own lexer over all 1621 rows finds it in **0** tree
files, against **2** in `~/app/ansible`. The two public-corpus hits are this fix's own
adversarial rows — which is the control, since a checker that found the shape nowhere would
report the same zero.

## Done when

- [x] a `{%` that opens nothing inside a raw body is data, asserted against jinja2 3.1.6's own
      split for the same four sources, with an unterminated raw still refused as the control
- [x] the `{%- endraw %}` and `{% endraw -%}` trims still reach the data on both sides
- [x] the surface is asserted through `did_open`, not only through the reader
- [x] both corpus gates green on the eight public trees and on `~/app/ansible`
