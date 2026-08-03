# T-029 — Hover showing the candidates tried

| Status          | Priority | Size | Depends on |
| --------------- | -------- | ---- | ---------- |
| **partly done** | P3       | S    | —          |

## Problem

`Resolution` already carries `candidates: Vec<PathBuf>` — every path tried, in order — and
nothing shows it unless the reference is missing. When resolution *succeeds* the information is
computed and thrown away.

That's the information you want in three situations:

- **which of several matches won.** Ansible resolves ambiguity silently and the order is
  unintuitive: the role's `tasks/` dir beats the including file's own directory. T-023 warns
  about this; hover explains it on demand without adding a diagnostic.
- **why a templated reference offered these candidates** and not others.
- **which collection a module came from** — in-repo, installed, or `ansible.builtin` — and
  whether it landed in `plugins/modules/` or `plugins/action/`. The
  documentation-only-module distinction is confusing enough that showing it is worth the hover.

## Approach

`textDocument/hover`. Resolved target first, then the candidates tried, in order, marking the
one that won. Skipped references show the reason instead — which doubles as the answer to
"why isn't this coloured." (The original reason list here named `RemotePath` and
`UnknownModule`; the post-parser-swap `SkipReason` enum has only `Templated` and
`NotInWorkspace`.)

Cheap: the data already exists, this is a formatter and a handler. Carries to Neovim for free,
unlike the decoration.

Keep it short — three or four lines. A hover that fills the screen gets dismissed reflexively.

## Progress

Landed in `39e5a4d`: the candidates hover with the winner marked, gated by
`hover.candidatesOnResolved` (default **off** — the target is a Cmd+click away) and
`hover.candidatesOnMissing` (default **on**). Hover resolves only the reference under the
cursor instead of every reference in the file.

Gap found live on `demo/tasks/main.yml:81` and closed: a **templated path whose variables
have no known value** used to get no hover at all — `path_substitution_hover` returned
`None` and nothing fell back — even though the resolver glob-matched targets (the
decoration said "2 possible targets", hover said nothing). Now templated refs fall through
to `reference_hover`, which lists the glob matches ("N possible targets", no winner — all
are equally possible until runtime) or explains the skip when nothing matches. An
ambiguous ref (≥2 targets) hovers regardless of `candidatesOnResolved`: no single target
is a click away, and the decoration only gives the count. Pinned by
`hover_lists_glob_targets_for_unknown_value_templated_paths` against the real demo.

## Done when

- [x] hover on a resolved reference shows the target and the candidates tried, in order
      (`reference_hover`, behind `hover.candidatesOnResolved`)
- [x] the winning candidate is marked
- [x] a skipped reference shows its skip reason: `NotInWorkspace` message; templated paths
      with unknown variable values list their glob-matched targets, or say why nothing
      matches (known-value substitution keeps its own richer hover)
- [ ] modules show which collection and whether it's `plugins/modules/` or `plugins/action/`
