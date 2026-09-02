# T-217 — A .j2 file has no syntax highlighting, because the extension contributes no language or grammar

| Status | Kind | Priority | Size | Epic  | Depends on |
| ------ | ---- | -------- | ---- | ----- | ---------- |
| partly done | task | P2  | M    | T-124 | —          |

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
| `#jinja2: block_start_string:'<%'` | `{%` hardcoded, so wrong in both directions | the header's real delimiters |
| a header's `line_statement_prefix` | a `#` line is a comment | it is a statement |
| delimiters from the `template:` call site | the defaults, always | what the render site passes, down the include tree |

**`{% raw %}` is not on that list, and an earlier draft of this ticket had it there.** A
TextMate `begin`/`end` rule *does* carry state across lines, so a raw body is handled correctly
by a grammar as long as its rule is ordered first. Measured, not argued — see the Progress
section. The claim was written from "regexes are stateless", which is true of a `match` rule
and false of a `begin`/`end` pair.

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


## Progress: the client half is in (`9b2e727`)

`.j2` opens as **Jinja**, not Plain Text. What shipped:

- `contributes.languages` — id `jinja`, extensions `.j2/.jinja/.jinja2`, which also makes the
  pre-existing `onLanguage:jinja` activation event real rather than dangling.
- `client/syntaxes/jinja.tmLanguage.json` — **written here, nothing vendored**, so the licence
  question the Approach raised does not arise. ~90 lines: the three delimiter shapes, strings,
  numbers, constants, filter-vs-variable, and the tag keywords `jinja::statement` reads.
- `client/language-configuration.json` — `{# #}` comment toggling, bracket pairs, auto-close.
- `client/test/grammar.js`, in `npm test` — 13 assertions, tokenised with `vscode-textmate`,
  the engine VS Code itself paints with, rather than read off the JSON.

### The three open decisions, settled

- **Language id `jinja`.** Nothing else on the dev machine contributed it — which is *why* the
  files were grey — so there was no collision to measure.
- **Written, not vendored.** No third-party licence enters the tree.
- **Jinja only; the base language is out of scope.** `app.conf.j2`'s conf text stays plain. The
  alternative is a grammar per base type (`.yml.j2`, `.conf.j2`, …), which is what the large
  Jinja extensions ship, and the port cannot help with any of it — it knows Jinja, not nginx.
  A decision, not an oversight.

### Seen red

The `#raw` rule is first in the pattern list, and that ordering is the whole fix. Demoted below
`#statement`: `{% raw %}` degrades to a generic tag with `raw` scoped as a **variable**, and the
raw-body assertion fails — while the control (*outside* a raw body the same delimiters *are* a
tag) stays green. So the test discriminates rather than merely passing.

### Why it reads as "partially" coloured

A grammar assigns **scope names**; the theme owns every colour. Resolved through Dark Modern's
real inheritance chain (`dark_modern.json` -> `dark_plus` -> `dark_vs`), our scopes land on:

| scope | resolves to | |
| ----- | ----------- | - |
| `comment.block` · `keyword.control` · `string.quoted` | `#6A9955` · `#569cd6` · `#ce9178` | painted |
| `constant.numeric` · `constant.language` · `support.function` | `#b5cea8` · `#569cd6` · `#DCDCAA` | painted |
| `variable.other` · `punctuation.definition.template-expression` | `#9CDCFE` · `#569cd6` | painted |
| `punctuation.definition.tag` | `#808080` | dim **by the theme's choice** |
| `keyword.operator` | `#d4d4d4` | equals the default foreground, so it reads as uncoloured |
| `meta.*` | nothing | structural by design; never painted by any theme |

So on Dark Modern only operators and the punctuation inside `{{ }}` render as plain text, and
both are the theme's own convention for those scopes in every language. Using the standard
prefixes rather than Jinja-specific ones is what makes any third-party theme work with no
further effort.

## Progress: the tokens are in, and the grammar stopped painting tags

`textDocument/semanticTokens` is served for `.j2`. `jinja::highlight` maps
[`template::document_in`]'s blocks to typed spans, `semantic_tokens_of` encodes them, and the
legend is seven standard LSP names plus one that is not — see below. Measured in the editor on
`demo/templates/overridden.conf.j2`: **`<% include "partials/header.j2" %>` colours as a real
tag and the literal `{% notatag %}` does not**, which is the pair that was exactly backwards.

### The grammar had to stop painting tags, because a token cannot un-paint

The first attempt kept the grammar and added a `Text` token over data regions, mapped through
`contributes.semanticTokenScopes` to an uncoloured scope, to override the grammar where it was
wrong. **It does not work.** The token was emitted — confirmed on the wire, `Text "\n{% notatag
%}\n"` — and the editor went on painting the literal braces as a tag.

A semantic token overrides a grammar scope **only where it provides one**, and mapping to a
scope with no colour rule falls through to the TextMate scope rather than clearing it. "This is
ordinary output" is not sayable. So silence cannot correct a grammar's guess, and the only fix
is for the grammar not to guess: `#statement`, `#comment` and `#raw` are gone. What is left
paints `{{`/`}}` as neutral punctuation, which is true whatever the delimiters are.

The mechanism does work in the other direction — *adding* a colour to a token type the theme
has no rule for. That is how the `delimiter` type below keeps the grey the grammar used to give
`{%`, and it is what VS Code's own TypeScript extension uses it for.

Cost accepted: a `.j2` is unpainted until the server answers. Measured at **13-43µs** per
request in the editor, so it is not perceptible.

### Two bugs the work turned up, neither of them about colour

**`did_open` cleared the grammar cache.** `invalidate_render_sites` drops
`template_grammars` wholesale, and `did_open` called it — so opening a tab threw away a walk of
every YAML file and every template, which `publish_diagnostics` then rebuilt inline, once per
tab. Opening a file changes nothing: the buffer that arrives is what is already on disk, and
both caches derive from disk. Removed there, kept in `did_change`/`did_save`. Measured over
`demo` with 41 files open: **268ms -> 125ms**, and it was slowing diagnostics, not just colour.

**The delimiters were dropped, then emitted out of order.** `Block` carries `span` (with the
delimiters) and `inner` (without); the first version read only `inner`, so `{%` and `%}` stopped
being painted by anything once the grammar's rules were removed — a regression against the same
morning, missed because every test asserted what was *inside* a tag. The fix then pushed both
delimiters before the content, and **the protocol encodes each token as a delta from the
previous one**, so the column subtraction underflowed and the encoder panicked. Three existing
tests caught it, which is the argument for asserting the wire format rather than the reader's
return value. `tokens()` now carries a `debug_assert!` that output is in source order.

### The 10-15s before colour appeared was VS Code, not this

Chased through four wrong hypotheses before asking for the log, which settled it in one line:

```
[client] client.start() resolved in 13 ms (activate reached at 1494 ms uptime)
5:08:31  ready -> detect 8ms -> scan 115 files 12ms -> semanticTokens 18.292us
```

Everything from activation to painted text is ~1.5s, and 1.49 of it is the extension host
reaching `activate`. The startup report named the cause in its first block — **`Has 7 other
windows`** — and VS Code restores them serially, which is what "window by window, then the text
window" looks like. `window.restoreWindows: "one"` is the user-side fix.

Recorded because the wrong lesson is available here: the probes said 291ms and the editor said
15s, and the gap was neither the server nor the client. Ask for the log, and ask what the screen
is actually doing, before theorising about the code.

## Progress: YAML scalars are painted too

`name: "{{ app_name }}"` in a playbook now colours. The handler serves `.yml`/`.yaml` as well
as the three template extensions; anything else still gets `None`, so a `.md` full of braces is
not painted with Jinja's legend. `Backend::yaml_scalar_tokens` walks the tree and runs the same
`jinja::highlight` builder per scalar — which is what it was made span-based and
delimiter-parameterised for. `root: false` throughout: a scalar cannot carry a `#jinja2:`
header, so its delimiters come from the file's render site and never from itself.

### The offsets are guarded by an identity, not by a list of scalar styles

A scalar's value is only sometimes its source text. Measured before anything was built:

| written | `value` | source slice | 1:1? |
| ------- | ------- | ------------ | ---- |
| `plain {{ a }}` | `plain {{ a }}` | same | yes |
| `"dq {{ b }}"` | `dq {{ b }}` | same — the span excludes the quotes | yes |
| `'sq {{ c }}'` | `sq {{ c }}` | same | yes |
| `>` folded | `folded {{ d }}\n` | `>\n    folded {{ d }}\n` | **no** |
| `\|` literal | `literal {{ e }}\n` | `\|\n    literal {{ e }}\n` | **no** |

So `span.start + offset` is valid for the first three and garbage for the last two — the
indicator and the block indent are stripped from the value, and an escape shortens it. Rather
than enumerate styles, the walker requires the identity that makes the arithmetic valid:
`text[span] == value`, or it paints nothing.

Seen red, and the failure is the argument for the guard. With it removed, the block-scalar
fixture paints a `delimiter` on the `\|` indicator itself and a `variable` at column 1 of the
next line:

```
(3, 7, 1, "delimiter"), (4, 1, 7, "variable"), (4, 9, 2, "delimiter")
```

Note this same `base + offset` assumption is what `vars::uses` makes (`vars.rs:308`), so
variable spans inside a block scalar are likely off in hover and go-to-definition too. Not
touched here — a separate finding, and it wants its own measurement of which consumers show it.

### `when:` is read as an expression, not as a template

The decision this section asked for. A bare `when: flag is defined` has no `{{ }}`, so reading
it as a template paints nothing at all. Ansible wraps a `when:` in `{{ }}` itself before
evaluating, which is why `vars::uses` already treats one as an expression — `jinja::highlight`
grows `expression_tokens` and the walker uses it under a `when:` key, so both readers answer
from one rule about what a `when:` *is*. Verified: `flag` comes out a variable, `is`/`defined`
keywords.

Checked end to end against `demo/tasks/main.yml` — 27 tokens, each one slicing back out of its
own line as exactly the text the token claims.

## Still to do

Ordered deliberately, and the order is the point: **richer types before modifiers.** The two
look like neighbours — both "say more about a token" — but they differ in what they *claim*,
and that decides which is safe to build on today.

A richer **type** describes the text we already parsed: this name is a property, this one is a
loop binding. Getting it wrong shows a slightly wrong colour. A **modifier** saying a variable
does not resolve is a claim about the whole workspace, and getting it wrong fades a variable
that exists — the same class as a false diagnostic, which is what this project's first
paragraph is about. The resolver underneath is not ready to make that claim: role entries are
modelled wrong ([[T-063]] — `main` treated as a fixed target rather than `tasks_from`'s
default), variable spans inside a block scalar are probably off (see the YAML progress note
above), and a non-ASCII name crashed the request outright until [[T-219]]. Fading a name on top
of that advertises those gaps as visible wrong colour on every file.

- **Richer token types.** `h.name` as a property, a `{% for h in … %}` binding versus a lookup,
  an `{% import … as m %}` namespace, a `{% macro %}` definition. All standard LSP types.

  **Partly done in `1e8fc7e`, on a different axis than the slices below.** That commit split
  the single `Keyword` kind into tag names, word operators (`and`/`or`/`not`/`in`/`is`) and
  literals, adding `WordOperator` and `Constant` to the enum and the legend, so a theme can
  tell them apart. It does *not* touch attribute access, loop bindings or definitions.
  Recorded so they are not re-derived.

  **Slice A landed.** `Property` is in the enum and at legend index 11 as the standard
  `property` type, so the client maps nothing. The arm sits *after* the call check on purpose:
  `m.upstream(` is a call first, and the demo fixture asserts both halves —
  `the_chain_root_demo_paints_the_dotted_name_as_a_property` reads
  `demo/templates/app.conf.j2`, where `ansible_facts.hostname | default(inventory_hostname)`
  is the property and `m.upstream('web')` is the control. Measured in the bundled themes
  rather than assumed: Dark Modern inherits dark_plus's single `variable` rule (`#9CDCFE`)
  and has no `variable.other.property` rule, so `property` and `variable` paint the **same**
  colour there. The token is distinguishable, not yet distinguished — a theme (or an
  `editor.semanticTokenColorCustomizations` rule for `property`) is what makes it visible.
  B and C below are still open.

  **Correction to an earlier draft: these are not "already in the AST" as far as this code path
  is concerned.** `inner_tokens` reads `lexer::tokens`, not the parser, and classifies by
  neighbour — `prev == Pipe || next == Lparen` is how a filter and a call become `Function`
  today. So this is more heuristics of the same shape, not a mapping from an AST that is
  already being walked. Worth knowing before sizing it.

  Sliced smallest-first, since each stands alone:

  | slice | change | most visible on |
  | ----- | ------ | --------------- |
  | A | `prev == Dot` -> `Property` — one match arm | `{{ ansible_facts.hostname }}`, which is everywhere in real playbooks |
  | B | names between `for` and `in` are bindings, not lookups | `{% for h in hosts %}` |
  | C | `{% macro %}` name as a definition, `{% import … as m %}` as a namespace | macro-heavy templates |

  A and B stay lexer-local. C wants statement-position state and is the first that might earn
  the parser.

- **Token modifiers.** The legend's modifier list is empty on purpose. Whether a variable
  *resolves* is a property of a token rather than a kind of token, and it is the one thing here
  no other Jinja tooling can answer — the same slot [[T-126]] needs.

  Blocked on judgement, not on code, for the reason above. Two costs to settle first, both
  structural rather than incremental:

  - `highlight::tokens(src, delimiters, root)` takes **no workspace and no resolver**. Semantic
    tokens are requested on every keystroke, so this means resolving per edit or designing a
    cache — a change to the function's shape, not an added arm.
  - The modifier design is shared with [[T-126]]. Whichever lands first owns it, so they should
    be settled together rather than one inventing a scheme the other has to adopt.

- **Packaging.** The extension only loads under F5. A stale hand-copied build in
  `~/.vscode/extensions/` was shadowing this and has been removed, so a plain `code .` window
  now has no extension at all. A `.vsix` build-and-install step is needed for the editor to run
  this outside the debug host — the same class of trap as rule 6, one level up from the server
  binary.

## Done when

- [x] a `.j2` in `demo/templates/` is coloured with no other Jinja extension installed,
      verified in a real editor rather than asserted from the manifest
- [x] the language id and file extensions are pinned by a test in the `client/test/selector.js`
      shape — the manifest read back and matched against every `.j2` in `demo/`, so a renamed
      extension or a dropped glob is caught without a human opening VS Code
- [x] `activationEvents` names only ids this extension actually contributes, or the entry that
      does not is removed
- [x] the licence of any vendored grammar is recorded here, with its source and revision
      — n/a, resolved by writing one instead: nothing vendored, no third-party licence in the tree
- [x] whether semantic tokens need a base grammar is **measured** — a token overrides a grammar
      scope only where it provides one, and cannot clear one, so the grammar stopped painting tags
- [x] a `{%` inside a `{% raw %}` body is not coloured as a tag — the [[T-216]] shape, which is
      the case that justifies serving tokens at all
