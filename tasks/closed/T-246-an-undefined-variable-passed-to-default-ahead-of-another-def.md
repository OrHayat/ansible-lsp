# T-246 — An undefined variable passed to default() ahead of another default() is reported undefined, and the chain runs fine

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | bug  | P1       | M    | T-112 | —          |

## Symptom

`var-undefined` fires on `artifact_source` in

```yaml
_demo_source: "{{ demo_source | default(artifact_source) | default('internal') }}"
```

with neither `demo_source` nor `artifact_source` defined. Measured on ansible-core 2.21.2, the
same shape (`awscli_source | default(artifact_source) | default('internal')`, both undefined)
renders `internal`, with no error and no warning. The undefined argument reaches the second
`default`, which replaces it.

Found while closing T-226. On `volumez/matrix/ansible` at `12d9c16e` there are 31
`artifact_source` reports, and every one I read has this shape: `demo-download.yml:24,216,217`,
`spark-setup.yml:74,75`, `alluxio-setup.yml:147,148`, and more. Not all 31 were read.

## Cause

`condition::walk_guards` covered a name only when it sat directly under a `default` or a
definedness test. In Ansible 2.21.2 an undefined value is not an error until something uses
it. It passes up through attribute access, subscripts, every filter except `mandatory` (as the
input or as an argument), and method calls, then fails at an operator, a comparison,
`mandatory`, or the render. Jinja's `d` alias for `default` was also not recognised, so
`a | d('z')` was reported.

## Resolution

Measured first: 17 shapes in `edges.yml`, 18 in `filters.yml`, plus a `hostvars` case and a
`when:` case, all on 2.21.2. Then:

- An argument of `default` keeps the guard on that default's own result, so
  `a | default(y) | default('z')` covers `y`, and `a | default(y)` on its own still does not.
- A filter other than `mandatory` passes the guard down to its input and its arguments.
- A method call passes it down to its receiver. Its arguments were not measured and stay
  uncovered.
- `d` is `default`.

The rule lives in `walk_guards`, so all three readers of `guard_at` get it: the undefined
rule, `is_guarded` for `when:` clauses, and `expression_swallows_undefined` for `hostvars[...]`.
`an_undefined_value_is_reported_only_where_ansible_fails_on_it` pins 22 measured rows, and
`a_filter_before_the_default_keeps_the_guard_for_every_reader` covers the other two readers.
Both were seen red with the change reverted.

On the reference tree, reports go from 272 to 241. The 31 that went are exactly the
`artifact_source` reports, every one a `default(artifact_source) | default(...)` chain, and
nothing else changed.

## Done when

- [x] the `demo-download.yml:24` shape is not reported, and a test pins it
- [x] `default(x)` as the last link, with `x` undefined, is still reported if measurement says
      it fails, and a test pins whichever answer is measured. It fails, and it is reported.
- [x] the reference tree's `artifact_source` reports are re-read, and every one that remains is
      a use measured to fail. None remain.
