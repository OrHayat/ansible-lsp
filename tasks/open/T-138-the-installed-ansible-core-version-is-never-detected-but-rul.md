# T-138 — The installed ansible-core version is never detected, but rules need it

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | task | P1       | S    | —          |

## Problem

`AnsibleInstall` records where Ansible is, never which Ansible it is:

```rust
pub struct AnsibleInstall {
    pub package_dir: Option<PathBuf>,
    pub collection_roots: Vec<PathBuf>,
    pub source: Source,
    pub detect_ms: f64,
}
```

Ansible's own behaviour is version-gated in ways we already model or plan to, so rules
written against one version are silently wrong on another. **T-117 is already written
against a capability that does not exist** — its Approach says "gate anything
version-sensitive on the detected ansible-core version — the same detection T-084 already
put on a background thread", but no version is detected anywhere. Whoever picks up T-117
hits this first.

Known version-sensitive behaviour on the board today:

| Since | Behaviour | Ticket |
| ----- | --------- | ------ |
| 2.19 | `when:` is strict — empty and non-boolean conditions became fatal | T-117 |
| 2.19 | fully-wrapped `when: "{{ x }}"` deprecated, removal slated 2.23 | T-117 |
| 2.23 | `ALLOW_BROKEN_CONDITIONALS` scheduled for removal | T-117 |
| 2.15 | PR #80171 made the `vars_files` missing-file raise unreachable | Settled |
| 2.18 | PR #83259 deleted that dead raise | Settled |
| 2.3 | implicit fact gathering skipped with a gated `import_playbook` | Settled |
| 2.10 | the collection-split routing table | T-064, T-083 |

Applying a 2.19 rule to someone on 2.16 is the tool lying, which is what P1 is for.

## Where every `--version` field comes from

`--version` **computes almost nothing** — `version()` (`cli/arguments/option_helpers.py:285-322`)
prints constants already resolved at import time. Worth having in one place, because it is
the natural thing to reach for and mostly the wrong source:

| Field | Where it comes from | Static equivalent |
| ----- | ------------------- | ----------------- |
| `[core X]` | `ansible.release.__version__` | **read `release.py`** — plain file |
| `config file` | `C.CONFIG_FILE`, set at import by `find_ini_config_file` | exactly what T-098 must replicate |
| `configured module search path` | `C.DEFAULT_MODULE_PATH`, or the literal `"Default w/o overrides"` when unset | the `library` cfg key — `config.rs:61` ✅ |
| `ansible python module location` | `ansible.__path__` | our walk-up ✅ |
| `ansible collection location` | `C.COLLECTIONS_PATHS` | env + cfg + defaults ✅ |
| `executable location` | `sys.argv[0]` — raw argv, **not resolved** | `which("ansible")` |
| `python version` | `sys.executable` | the shebang of the `ansible` script |
| `jinja version` | `_templating.jinja2_version` | jinja2's dist-info in site-packages |
| `pyyaml version` | `yaml.__version__` + `HAS_LIBYAML` | needs a Python import — **out of scope**, see below |

Three of these are already modelled (✅). `config file` is T-098's whole subject — the
`--version` line is a *report* of `find_ini_config_file`, so it is a good oracle to test
T-098 against, not a source to read from.

`executable location` deserves a warning: it is `sys.argv[0]` verbatim
(`option_helpers.py:317`), so running `./ansible` prints `./ansible`. It looks like a
discovered path and is the least authoritative line in the output.

**`pyyaml version` is deliberately out of scope.** The `(with libyaml vX)` fragment comes
from `HAS_LIBYAML`, decided by whether `import yaml.cyaml` succeeded in that interpreter —
the only field with no on-disk source. It is the premise under T-036's Settled entry (we use
`libyaml-safer` because it accepts what Ansible accepts, which holds when PyYAML has
libyaml), but there is nothing actionable to do with the answer, so it is not being detected.
Recorded so the question is not reopened.

## Approach

One field, `version: Option<Version>`, filled on **both** detection paths.

**Filesystem path (the one always taken).** `<package_dir>/release.py` holds
`__version__` — a plain assignment in a small file, so one read and a regex. This is the
same source `--version` prints: `option_helpers.py:288` formats
`ansible.release.__version__` into `ansible [core X.Y.Z]`. No subprocess, so it works on
the fast path and on Windows, where the CLI cannot run at all.

**Version-command path.** Take it from the first line, which is already being read.

Parse into a comparable triple; ansible-core uses plain `X.Y.Z` releases plus `.devN` /
`rcN` suffixes (`2.22.0.dev0` appears in the upstream dossiers), so parse the numeric head
and ignore the rest rather than pulling in a semver crate for it.

## Watch out

- **No install detected means no version.** `Option`, and every gated rule must decide what
  it does with `None` — the safe default is to apply the *current* behaviour, since a
  workspace with no local Ansible is usually a CI-targeted repo on a recent core. Say so at
  each call site rather than once here.
- Do not reach for `ansible --version` to get this. It costs 3.6 s cold (T-084) and crashes
  on Windows; `release.py` is a file read on the path we always take.
- `executable location` in `--version` output is `sys.argv[0]` verbatim
  (`option_helpers.py:317`) — not resolved, not canonical. Not a source for anything.

## Done when

- [ ] `AnsibleInstall::version` is populated from `release.py` on the filesystem path
- [ ] and from the first line on the version-command path
- [ ] a fixture pins parsing of a `.dev` / `rc` suffix, not just `X.Y.Z`
- [ ] `None` is handled explicitly wherever a rule gates on it
- [ ] T-117's dependency is satisfied — it can gate its strict-`when:` rules
