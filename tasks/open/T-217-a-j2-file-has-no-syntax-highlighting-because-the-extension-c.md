# T-217 — A .j2 file has no syntax highlighting, because the extension contributes no language or grammar

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| open   | task | P2       | M    | T-124 | —          |

## Problem

Open any `.j2` in the demo tree and it is uniform grey. Not a theme quirk and not a
regression — nothing in this extension has ever told VS Code what a `.j2` file *is*.

`client/package.json`'s `contributes` block has **no `languages` entry and no `grammars`
entry**. VS Code ships no built-in language id for `.j2`, so the file opens as Plain Text,
and Plain Text has no grammar to paint with.

The extension already knows this and works around it for a different purpose —
`client/src/extension.js:795`:

> By glob, not by language id: VS Code ships no id for `.j2`, so which one a
> file gets depends on what else is installed.

That glob gets the **server** to see the file, which is why diagnostics, hover and
go-to-definition all work in a document that has no colour at all. Colour is a `contributes`
concern, not an LSP one, so the parser was never wired to anything that paints.

Two consequences worth naming:

- `activationEvents` carries `onLanguage:jinja`, an id **this extension does not contribute**.
  It fires only if some other installed extension happens to define it. The
  `workspaceContains:**/*.j2` entry beside it is what actually activates us today.
- Whatever colour a user does see in a `.j2` comes from another extension entirely, so the
  project has no control over it and no way to be right about `{% raw %}`.

## Approach

Two layers, and the second is the one only this project can do.

### 1. A `languages` + `grammars` contribution

The standard fix: register a language id for `.j2/.jinja/.jinja2` and ship a TextMate grammar.
Colour appears immediately, keeps working while the server is starting or down, and repaints
mid-keystroke on a file too broken to parse — none of which a server round-trip can offer.

Decisions to settle before writing any of it:

- **Which language id.** `jinja` is what `activationEvents` already names, and registering it
  would make that dangling entry real. But if the user has another Jinja extension installed,
  two extensions contributing the same id is its own failure mode; measure what happens rather
  than assume.
- **Vendor a grammar or write one.** Licensing is the constraint, not effort: this repo
  carries no LICENSE, and T-077 already had to pick a corpus repo on those grounds. A
  vendored `.tmLanguage.json` brings its own licence with it.
- **What about the file underneath.** A `.j2` is a template *of* something — `app.conf.j2`,
  `site.yml.j2`. Highlighting only the Jinja leaves the other 90% of the file grey. Deciding
  whether the base language is in scope changes the size of this ticket substantially, and
  "Jinja only" is a defensible first answer as long as it is a decision and not an oversight.

### 2. `textDocument/semanticTokens`, served from the real parser

Where the port earns its keep. A TextMate grammar is regexes and holds no state across lines,
so it is **structurally wrong** in three cases this project already gets right:

| case | what a grammar paints | what we know |
| ---- | --------------------- | ------------ |
| `{% raw %}printf "x{%s}"{% endraw %}` | `{%s}` as a tag | it is literal data — this is [[T-216]] as colour |
| `#jinja2: block_start_string:'<%'` | `{%` hardcoded, so wrong in both directions | the header's real delimiters |
| a header's `line_statement_prefix` | a `#` line is a comment | it is a statement |

Delimiters also arrive from the `template:` call site and inherit down the include tree
(`98b205f`), which no static grammar can follow.

The server advertises `definition_provider`, `hover_provider` and `document_link_provider`
today — there is no `semantic_tokens_provider`, so this is a new capability, not a new
analysis. `jinja::template`'s split already produces the spans.

**Unverified and load-bearing:** whether VS Code paints semantic tokens on a document whose
language has no TextMate grammar at all, or whether tokens only ever *refine* grammar scopes.
If the latter, layer 2 cannot un-grey a file on its own and the ordering above is forced
rather than merely convenient. Run it before designing around either answer — rule 1.

### Relationship to [[T-126]]

[[T-126]] is semantic tokens for the **teal reference decorations**, scoped to surviving the
move to Neovim. It is not this ticket and does not cover Jinja syntax. But both add the same
capability to the same server, so the token-type legend, and full-vs-range requests, want
settling **once** — whichever lands first owns that decision and the other cites it.

## Done when

- [ ] a `.j2` in `demo/templates/` is coloured with no other Jinja extension installed,
      verified in a real editor rather than asserted from the manifest
- [ ] the language id and file extensions are pinned by a test in the `client/test/selector.js`
      shape — the manifest read back and matched against every `.j2` in `demo/`, so a renamed
      extension or a dropped glob is caught without a human opening VS Code
- [ ] `activationEvents` names only ids this extension actually contributes, or the entry that
      does not is removed
- [ ] the licence of any vendored grammar is recorded here, with its source and revision
- [ ] whether semantic tokens need a base grammar is **measured**, and the answer written into
      the Approach above in place of the open question
- [ ] a `{%` inside a `{% raw %}` body is not coloured as a tag — the [[T-216]] shape, which is
      the case that justifies serving tokens at all
