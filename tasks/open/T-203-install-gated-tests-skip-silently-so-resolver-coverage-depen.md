# T-203 — Install-gated tests skip silently, so resolver coverage depends on the machine

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | task | P2       | M    | —          |


## Problem

Tests decide whether to assert by asking the machine:

```rust
if AnsibleInstall::detect(None).package_dir.is_none() { return; }
```

Enumerated rather than guessed - a first count said "three", and a grep found **eight**, in
both crates:

| site | gate |
| ---- | ---- |
| `install.rs:560` `finds_the_local_ansible_install` | `package_dir.is_none()` |
| `resolve.rs:1292` | `package_dir.is_none()` |
| `resolve.rs:2323` `builtin_modules_resolve_into_the_installed_ansible` | `package_dir.is_none()` |
| `resolve.rs:2414` `installed_collection_modules_resolve` | `collection_roots.is_empty()` |
| `main.rs:5649` (ansible-lsp) | `package_dir.is_none()` |
| `parse_libyaml.rs:426` | `env::var("ANSIBLE_CORPUS")` |
| `vars.rs:3528`, `vars.rs:3629` | `env::var("T076_ROOT")` |

The last three are the same shape against a different input: an environment variable rather
than an install. They belong in the same sweep even if the fix differs - a corpus path is not
something to vendor, but "silently asserts nothing" is the same defect.

On a machine with no `ansible` on PATH they return before asserting anything, and report `ok`.
There is no way to tell that apart from a real pass.

This is the shape T-077 removed — *"guarded with `else { return }`, so they skipped silently on
any other machine — a green run proved nothing on CI or a fresh checkout"* — but **T-077 did not
miss these; it excluded them on purpose**, and said why:

> Note `builtin_modules_resolve_into_the_installed_ansible` and `installed_collection_modules_resolve`
> are a *different* dependency — the machine's Ansible install, not the repo — and are out of
> scope here. Their `repo()` guard was still dropped ... so they now skip for the one reason
> they actually have.

That was a reasonable call at the time: an Ansible install is reproducible in a way one
person's `~/app/ansible` is not, and T-077 narrowed the guard from two reasons to one rather
than leaving it alone. What has changed is evidence, not principle — the remaining reason turns
out to be enough to hide a real gap (below), on the primary dev machine rather than on some
hypothetical CI.

### Measured, and it hid a real gap

Windows dev box, no `ansible` on PATH; the working ansible-core 2.21.2 lives in WSL, where the
repo's live verification is done. While doing T-201 box (4) — moving the install off a process
global and onto `FileContext` — the whole workspace suite stayed green **with the install
removed from the resolver entirely**. The change was unverified and looked verified.

Two tests written against a synthetic package dir now cover the plumbing
(`a_builtin_module_resolves_through_the_install_on_the_context`,
`the_install_a_cache_carries_reaches_the_contexts_it_builds`), and each break fails exactly one
of them. That closes T-201's hole but not this one: the three guards are still there, and any
future test that reaches for a real install will reach for the same guard.

A second thing the same session found, which is why "just fake it" is not the whole answer: the
first version of the synthetic test covered only `ansible.builtin.ping`, which takes the
three-part branch of `resolve_module` (`:656`). The **bare** `ping:` spelling takes a different
branch (`:629`) and stayed green when that branch was broken. One spelling is not one code path.

## Approach

Point the tests at a **pinned Ansible tree in the repo** rather than at whatever the machine
has, so they always run and always run against a known version.

Sizes, measured against the WSL install (ansible-core 2.21.2):

| | size | what the resolver needs from it |
| - | ---- | ------------------------------- |
| the `ansible/` package | 13 MB | what `package_dir` points at |
| `modules/` | 1.4 MB, 74 files | only the file *names* — resolution asks whether the file exists |
| `config/ansible_builtin_runtime.yml` | 368 KB | real content: this is the redirect table `builtin_module_redirect` parses |

Options, to be priced before choosing:

- **Git submodule of `ansible/ansible`, pinned to a tag** (no precedent here — see the extract
  option below, which is what T-077 chose for the same question), tests pointing at `lib/ansible`.
  Real tree, no faking, and the pin is a feature: this repo already gates on core versions and
  says "measured on 2.21.2" throughout, so which version the tests ran against becomes a fact
  rather than a property of the developer's laptop. The cost is clone size — the source repo
  with history is far more than the 13 MB above, so this wants `--depth 1` or a sparse checkout
  of `lib/ansible`, and that needs measuring rather than assuming.
- **A vendored extract**: the real `ansible_builtin_runtime.yml` plus empty files named after
  the modules. ~400 KB, no submodule friction. But the module files become a fixture, and the
  existing comment at `resolve.rs` argues against exactly that for the *detection* tests —
  "faking the target would mean inventing a site-packages tree, and the test would then prove
  the fixture rather than the resolver".

  **This is the house style, and that matters more than it first looks.** T-077 hit the same
  question for the corpus tests and answered it this way: four `ansible-collections` repos plus
  kubespray, pinned by commit, with a shape-diverse subset **inlined verbatim** as
  `condition.rs`'s `REAL_WHENS` rather than vendored whole or added as submodules. It paid for
  itself immediately — 3 false positives (T-139) and a whole misread class (T-140). A pinned
  extract here would be the same move against a different input, and it is the option with
  precedent in this repo.
- **Keep the ambient install, but make skipping visible**: `#[ignore]` with a reason, or a
  `println!`. Cheapest, and strictly better than today, but it leaves coverage depending on the
  machine — it only stops the suite from lying about it.

Whichever is chosen, the guards go. A test that cannot assert should say so or not exist.

### Boundary worth keeping straight

A vendored tree gives **source, not execution**. Resolver tests ask "does this file exist in the
install tree", and source answers that completely. Rule 1 — "a claim about Ansible is not true
until it has been run" — is about *behavioural* claims, and those still need a real
`ansible-playbook` run (in WSL, on this setup). This ticket replaces the ambient-install
dependency; it does not replace live verification, and must not be read as doing so.

## Done when

- [ ] the five install gates in the table above are gone - `resolve.rs` x3, `install.rs`,
      and `main.rs` in the ansible-lsp crate
- [ ] the tests they guarded run on a machine with no `ansible` on PATH, and are shown to fail
      when the resolution they cover is broken (rule 5) — measured on this Windows box, which
      is the environment that hid the problem
- [ ] the chosen option is recorded here with its measured cost, including the clone size if a
      submodule is chosen — the reason for rejecting the other two written down, not implied
- [x] a sweep for the same shape elsewhere - done, the table above; **eight** sites, not the
      three first counted, and three of them gate on an env var rather than an install
- [ ] the three env-gated sites are decided too: a corpus path is not something to vendor, so
      each either becomes visibly skipped or stops pretending to be a test
- [ ] both module spellings covered wherever an install is involved — bare and FQCN take
      different branches of `resolve_module`, and one is not evidence for the other
