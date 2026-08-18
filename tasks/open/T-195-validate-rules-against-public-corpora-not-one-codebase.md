# T-195 — Validate rules against public corpora, not one codebase

| Status | Kind | Priority | Size | Depends on |
| ------ | ---- | -------- | ---- | ---------- |
| open   | task | P2       | M    | —          |

## Problem

## Approach

## Done when

- [ ]

## Problem

Every corpus gate in this repo reads one codebase. "0 new warnings across 759 files" has been
treated as "safe", when it means "safe for one team's habits". That is not hypothetical — it
cost real time twice this week:

- **T-177** sat blocked on a corpus gate that no corpus can satisfy. The tree has exactly one
  `add_host`, it is templated, and it defines no variables. Six large public repos measured
  0, 0, 0, 1, 3, 4. `add_host` is simply rare in production Ansible, and one repo could not
  reveal that.
- The distribution is visibly skewed: **831 `set_fact` to 1 `add_host`**.

Several rules now carry "count the hits before shipping" gates — T-184, T-185, T-190, T-191,
T-192, T-193 — and one codebase cannot discharge any of them. A hint validated against a single
repo ships as noise everywhere else.

## Approach

Keep `~/app/ansible` as the primary corpus and add public ones as a second gate. Already cloned
and measured into the scratchpad, so the selection work is done:

| repo                     | yml files |
| ------------------------ | --------- |
| ansible/ansible (tests)  | 2124      |
| openstack-ansible        | 1708      |
| debops                   | 1208      |
| kubespray                | 584       |
| ceph-ansible             | 297       |
| ovirt-ansible-collection | 239       |

They are unrelated to each other in authorship, house style and domain, which is the property
being bought. `ANSIBLE_CORPUS` already exists as the hook (`parse_libyaml::corpus_smoke`).

Constraints worth stating up front:

- corpora are **never committed** — cloned locally, referenced by path
- pin commit SHAs, or a gate that passed yesterday fails today for reasons nobody changed
- ansible/ansible is its own test suite, not production code. Useful for coverage of rare
  constructs, misleading for frequency claims. Label it as such wherever a count is quoted.
- this multiplies the cost of every new rule by six. That is the point, but it should be a
  deliberate trade rather than a surprise — a rule that is noisy on one of six is a finding,
  not a failure, and the ticket for that rule decides what to do.

## Done when

- [ ] a documented way to point the gates at several corpora, not one
- [ ] SHAs pinned and recorded, so a run is reproducible
- [ ] at least one existing rule re-measured across all of them, with the per-corpus counts
      recorded — if a shipped rule is noisy on an unrelated codebase, that is the first thing
      this should find
- [ ] the ansible/ansible caveat written where the counts are read, not only here
- [ ] the corpus-gate boxes on T-184, T-185, T-190, T-191, T-192 and T-193 updated to say
      *which* corpora satisfy them
