# T-079 — "Extract to collection": move a local plugin/module and rewrite every reference

| Status | Priority | Size | Depends on |
| ------ | -------- | ---- | ---------- |
| open   | P3       | L    | T-020, T-042/T-064 |

## Idea

An **opt-in, user-invoked refactor** that takes a local `library/` module or an
`action_plugins/`/other legacy plugin and moves it into a collection tree, rewriting **every**
reference across the workspace to the new FQCN in one step.

`ansible-creator` already scaffolds an empty collection skeleton (`galaxy.yml`,
`plugins/modules/`, `plugins/action/`, `meta/`, tests). What it does **not** do is relocate your
existing `library/foo.py` and fix the dozens of bare `foo:` call sites that point at it. That
gap is exactly what this server is positioned to close, because it already holds the reverse
index of every reference (T-020).

## Not a nag — explicitly

Legacy `library/`/`action_plugins/` dirs are fully supported and often the right choice for
in-house-only plugins (see T-073's discussion). So this is **never** a diagnostic, hint, or
suggestion — it is a command the user runs when they've *decided* to migrate. The tool must not
imply legacy is wrong.

## Approach (sketch)

- Command on a resolved local module/plugin: "Extract to collection `ns.name`".
- Use the reverse index (T-020) to find every reference — bare `foo:`, `ansible.legacy.foo`,
  `include_role`/`tasks_from`-adjacent uses — and rewrite them to the collection FQCN.
- Move the file into the target `plugins/<type>/` dir; leave the source dir clean.
- Report what it can't safely rewrite (templated names, references outside the workspace) rather
  than guessing — same discipline as the rest of the tool.

## Traps

- `module_utils` and plugin-to-plugin imports move with the plugin; a naive file move breaks
  them.
- A same-name plugin in another location (the T-073 shadowing case) means "which one am I
  moving" must be explicit.
- Tests and `meta/` that reference the old path.

## Done when

- [ ] a command extracts a `demo/library/` module into a collection under
      `demo/collections/ansible_collections/` and rewrites its call sites to FQCN
- [ ] references it can't safely rewrite are reported, not silently changed
- [ ] never surfaces as a warning/hint — invocation is explicit only
- [ ] the moved plugin still resolves (and the old bare name no longer does, by design)
