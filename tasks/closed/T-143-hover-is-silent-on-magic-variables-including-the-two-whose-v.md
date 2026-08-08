# T-143 — Hover is silent on magic variables, including the two whose value we detect

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| done   | task | P3       | S    | T-124 | —          |

## Problem

Hover a `{{ ansible_playbook_python }}` and nothing appears. The variable hover only answers
"where was this defined", and it bails before rendering when the answer is nowhere
(`main.rs:1007`):

```rust
if defs.is_empty() {
    return None;
}
```

For an injected name that is the correct answer to the question asked — no workspace file
defines it, which is exactly why the definedness rules exempt it (T-051). But it is the wrong
answer to the question a reader has, and for two names we now hold the value and say nothing:

| Name | Value we hold | Since |
| ---- | ------------- | ----- |
| `ansible_playbook_python` | `AnsibleInstall::python` | T-051's note, `628aa8b` |
| `ansible_version` | `AnsibleInstall::version` | T-138, `8842ff7` |

Two commits of detection with no consumer. This is the consumer.

## Approach

In `variable_hover_at`, before the empty-defs bail: if the name is one whose value the
detected install knows, render that instead of `None`.

**Only names with a value.** `inventory_hostname`, `ansible_os_family` and the rest of the
facts stay silent — a popup reading "provided by Ansible at runtime" on every fact token is
noise, and it would be the tool talking to hear itself. Widening later is additive; starting
wide cannot be undone quietly.

**Never trigger detection from a request.** `AnsibleInstall::detect()` is `get_or_init`, so
calling it on the hover path would run detection on the message pump the first time — the
3.6 s cold `ansible --version` freeze T-084 measured and T-075 fixed. Hover reads an
already-populated result or renders nothing: a `detected()` accessor over `OnceLock::get`.

**Say where the value came from.** It is the interpreter behind the Ansible the *editor*
found, not necessarily the one that will run the play (T-051's "a default, not a fact"). The
hover names the install so a wrong answer is diagnosable rather than mystifying.

`ansible_version` is a dict at runtime (`full`, `major`, `minor`, `revision`, `string`), so
the hover shows the release and does not pretend the bare name is a string.

Rendering goes through `md::Md` — hand-assembled hover markdown was T-082, already closed.

## Done when

- [x] hovering `ansible_playbook_python` shows the detected interpreter
- [x] hovering `ansible_version` shows the detected core version
- [x] the hover names its source, so a stale or wrong install is diagnosable
- [x] facts and value-less magic names still hover nothing
- [x] nothing on a request path can trigger detection
- [x] rendering is pinned by a test that supplies a synthetic install, so it runs on a
      machine with no Ansible

## Outcome

**The first cut was dead code, and the test did not catch it.** It hung the new hover off the
empty-defs bail in `variable_hover_at`, reasoning that an injected name has no definition and
would land there. It never lands there: the function's *first* line is
`vars::uses(nodes).find(…)?`, and `vars::uses` drops magic and `ansible_*` names in the
tokenizer (`condition::variable_uses`), so no `VarUse` is ever produced and the `?` returns
first. Hover on `{{ ansible_playbook_python }}` would still have shown nothing, install or no
install. The test passed because it called the renderer directly and proved nothing about
reaching it — a whole-path assumption verified at one end only.

Caught by probing `vars::uses` on the demo line: `["base_url"]`, no `ansible_playbook_python`.

What landed instead:

- `condition::injected_uses` — the deliberate complement of `variable_uses`, returning exactly
  the names it drops for being injected. Both now share one `scan_words` tokenizer taking a
  `keep` predicate, so the two views can never disagree about where a word *is*, only about
  which words they want. `variable_uses`'s own behaviour is unchanged.
- `vars::injected_uses`, the same tree walk with that extractor threaded through.
- `Backend::injected_var_hover_at`, reached from the `else` of the ordinary-use lookup rather
  than from the defs check — the only point where an injected token can be recognised at all.

`AnsibleInstall::detected()` is the accessor the hover uses: `OnceLock::get`, never
`get_or_init`. Detection stays a startup-only cost and a hover arriving before startup
finishes renders nothing rather than freezing the pump.

Both hovers name the package dir they came from, and the interpreter one says outright that
the running play's interpreter may differ. That is T-051's "a default, not a fact" made
visible instead of buried in a ticket — the value is the editor's Ansible, and CI or a second
venv will disagree.

Two tests, because the failure above was the gap between them.
`an_injected_name_is_found_under_the_cursor` pins the *lookup* — the name is found under the
cursor with the span hover will highlight, in a `{{ }}` and in a bare `when:`, an ordinary
variable is not claimed by this view, and `vars::uses` still returns only `base_url` so the
false-"undefined" class the exemption prevents stays shut.

Pinned by `injected_var_hover_speaks_only_where_it_holds_a_value`, which builds a synthetic
`AnsibleInstall` rather than detecting one — it runs on a machine with no Ansible, unlike
`finds_the_local_ansible_install`, which can only early-return there. The negative half
matters as much as the positive: facts, `playbook_dir`, and a known name with an undetected
value all render nothing.

`demo/tasks/variables.yml` carries it, one line below the `var-undefined` case, so the demo
shows both halves of the same name — exempt from the diagnostic, and known to hover.

**Verified live** in the Extension Development Host, 2026-08-08 — the popup appears with the
detected values. Worth recording because the automated coverage stops one step short: the
end-to-end test supplies a synthetic install, so everything from cursor to markdown is pinned,
but `AnsibleInstall::detected()` returning something real, and VS Code rendering the markdown,
were only ever going to be confirmed by looking.
