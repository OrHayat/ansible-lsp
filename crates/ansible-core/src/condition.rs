//! What a `when:` says about a default run, and what's provably wrong with it.
//!
//! Not an evaluator — a matcher over a closed set of shapes, plus a variable extractor.
//! Of a real repo's thousands of conditions a large fraction match no shape here and must stay
//! [`Verdict::Unknown`]; the moment this guesses, anything built on it becomes
//! untrustworthy, which is what got the call-hierarchy tree scrapped.
//!
//! The `| default(D)` filter is what makes the rest tractable: it states the value when
//! the variable is unset, so the condition carries its own default-run answer without
//! resolving anything. That matters — Ansible has 22 variable precedence levels.

use crate::install::Version;

/// Jinja tests and filters that are not variable references.
const NOT_VARIABLES: &[&str] = &[
    // filters
    "bool", "int", "length", "trim", "lower", "upper", "list", "first", "last", "default",
    "string", "float", "join", "split", "unique", "sort", "map", "select", "reject",
    "selectattr", "rejectattr", "regex_replace", "regex_search", "regex_findall",
    "from_json", "to_json", "from_yaml", "to_yaml", "basename", "dirname", "realpath",
    "count", "sum", "min", "max", "abs", "round", "flatten", "combine", "dict2items",
    "items2dict", "difference", "union", "intersect", "ternary", "mandatory", "quote",
    "b64decode", "b64encode", "type_debug", "json_query", "replace", "indent", "batch",
    "path_join", "splitext", "expanduser", "relpath", "human_readable", "hash",
    // operators and tests
    "not", "and", "or", "in", "is", "if", "else", "true", "false", "none", "defined",
    "undefined", "changed", "failed", "succeeded", "success", "skipped", "match",
    "search", "version", "subset", "superset", "iterable", "mapping", "sequence",
    "number", "boolean", "even", "odd", "sameas", "escaped", "truthy", "falsy",
];

/// Variables Ansible always provides, so their absence from the workspace means nothing.
const MAGIC: &[&str] = &[
    "inventory_hostname", "groups", "group_names", "hostvars", "item", "omit",
    "play_hosts", "role_name", "role_path", "playbook_dir", "inventory_dir",
    "inventory_hostname_short", "ansible_check_mode", "ansible_verbosity", "vars",
    // Set even with no play/host/task (`vars/manager.py:457`); its value, when a config
    // exists, is what T-098's discovery records. Undefined only when no config was found
    // — a case the definedness rule cannot assume, so never flag it.
    "ansible_config_file",
];

/// Shared with the definedness diagnostic (T-051): a magic name must never be
/// flagged as undefined.
pub fn is_magic(name: &str) -> bool {
    MAGIC.contains(&name)
}

/// Strip one balanced enclosing pair of parens, if the whole string is wrapped.
///
/// `trim_end_matches(')')` cannot be used here — it eats the closing paren of a trailing
/// `default(...)`, which silently broke every guarded comparison.
fn strip_outer_parens(s: &str) -> &str {
    let t = s.trim();
    if !(t.starts_with('(') && t.ends_with(')')) {
        return t;
    }
    let mut depth = 0usize;
    for (i, c) in t.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                // The opening paren closes before the end, so it isn't a wrapper.
                if depth == 0 && i != t.len() - 1 {
                    return t;
                }
            }
            _ => {}
        }
    }
    if depth == 0 {
        t[1..t.len() - 1].trim()
    } else {
        t
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// `not (skip_x | default(false) | bool)` — 80% of import-level conditions here.
    UnlessSet { var: String },
    /// `x | default(true) | bool`
    UnlessCleared { var: String },
    /// `x | default(false) | bool` — the flag has to be turned on.
    OnlyIfSet { var: String },
    /// `mode | default('native') == 'native'`. `matches_default` is whether the
    /// defaulted value satisfies the comparison, i.e. whether this runs when unset.
    WhenEquals {
        var: String,
        value: String,
        negated: bool,
        matches_default: bool,
    },
    /// `mode in ['a', 'b']`
    WhenIn {
        var: String,
        values: Vec<String>,
        negated: bool,
    },
    /// `x is defined` / `x is not defined`. Statically this is the interesting one:
    /// if `x` is defined nowhere in the workspace, the branch can never be taken.
    RequiresDefined { var: String, negated: bool },
    /// `x | default('') | length > 0`
    RequiresNonEmpty { var: String },
    /// Literal `when: false`.
    Never,
    /// Literal `when: true` — the condition has no effect at all.
    Always,
    /// Several clauses ANDed. `unreadable` counts the ones that matched no shape, so
    /// the label can say the summary is partial instead of implying it is complete.
    All {
        parts: Vec<Verdict>,
        unreadable: usize,
    },
    Unknown,
}

impl Verdict {
    /// Inlay text, or `None` when there's nothing honest to say.
    ///
    /// Wording is "unless X is set" rather than "runs by default": `default(D)` only
    /// gives the value when *unset*, and whether it's set somewhere — group_vars,
    /// inventory, `-e` — is not knowable from the condition.
    pub fn label(&self) -> Option<String> {
        Some(match self {
            Verdict::UnlessSet { var } => format!("runs unless {var} is set"),
            Verdict::UnlessCleared { var } => format!("runs unless {var} is false"),
            Verdict::OnlyIfSet { var } => format!("runs only if {var} is set"),
            Verdict::WhenEquals {
                var,
                value,
                negated,
                matches_default,
                // `matches_default` is the default-run answer, so it picks
                // "runs unless" vs "runs only if"; `negated` picks which side of the
                // comparison the change has to be on.
            } => match (matches_default, negated) {
                (true, false) => format!("runs unless {var} changes from {value}"),
                (true, true) => format!("runs unless {var} = {value}"),
                (false, false) => format!("runs only if {var} = {value}"),
                (false, true) => format!("runs only if {var} changes from {value}"),
            },
            Verdict::WhenIn { var, values, negated } => {
                let list = values.join(", ");
                if *negated {
                    format!("runs unless {var} is one of [{list}]")
                } else {
                    format!("runs only if {var} is one of [{list}]")
                }
            }
            Verdict::RequiresDefined { var, negated: false } => {
                format!("runs only if {var} is set")
            }
            Verdict::RequiresDefined { var, negated: true } => {
                format!("runs only if {var} is unset")
            }
            Verdict::RequiresNonEmpty { var } => format!("runs only if {var} is non-empty"),
            Verdict::Never => "never runs".to_string(),
            Verdict::Always => "always runs — this `when:` has no effect".to_string(),
            Verdict::All { parts, unreadable } => {
                // Inline space is tight, so name the first requirement and fold the rest
                // (further readable clauses plus unreadable ones) into a count.
                let reqs: Vec<String> = parts.iter().filter_map(|p| p.requirement()).collect();
                let first = reqs.first()?;
                let extra = reqs.len() - 1 + unreadable;
                if extra == 0 {
                    format!("runs only if {first}")
                } else {
                    format!("runs only if {first} +{extra} more")
                }
            }
            Verdict::Unknown => return None,
        })
    }

    /// This clause as a bare requirement, for joining with siblings. Deliberately drops
    /// the "runs unless" framing — that describes a whole condition, and a clause ANDed
    /// with others does not describe the whole condition.
    pub fn requirement(&self) -> Option<String> {
        Some(match self {
            Verdict::UnlessSet { var } => format!("{var} unset"),
            Verdict::OnlyIfSet { var } => format!("{var} set"),
            Verdict::UnlessCleared { var } => format!("{var} not false"),
            Verdict::WhenEquals { var, value, negated: false, .. } => format!("{var} = {value}"),
            Verdict::WhenEquals { var, value, negated: true, .. } => format!("{var} != {value}"),
            Verdict::WhenIn { var, values, negated } => format!(
                "{var} {}in [{}]",
                if *negated { "not " } else { "" },
                values.join(", ")
            ),
            Verdict::RequiresDefined { var, negated: false } => format!("{var} set"),
            Verdict::RequiresDefined { var, negated: true } => format!("{var} unset"),
            Verdict::RequiresNonEmpty { var } => format!("{var} non-empty"),
            Verdict::Never | Verdict::Always | Verdict::All { .. } | Verdict::Unknown => {
                return None
            }
        })
    }

    /// The variable this verdict hinges on, if it hinges on exactly one.
    pub fn var(&self) -> Option<&str> {
        match self {
            Verdict::UnlessSet { var }
            | Verdict::UnlessCleared { var }
            | Verdict::OnlyIfSet { var }
            | Verdict::WhenEquals { var, .. }
            | Verdict::WhenIn { var, .. }
            | Verdict::RequiresDefined { var, .. }
            | Verdict::RequiresNonEmpty { var } => Some(var),
            Verdict::Never | Verdict::Always | Verdict::All { .. } | Verdict::Unknown => None,
        }
    }

    /// Two branches on the same variable demanding different values can't both run.
    pub fn excludes(&self, other: &Verdict) -> bool {
        match (self, other) {
            (
                Verdict::WhenEquals { var: a, value: x, negated: false, .. },
                Verdict::WhenEquals { var: b, value: y, negated: false, .. },
            ) => a == b && x != y,
            (
                Verdict::WhenIn { var: a, values: x, negated: false },
                Verdict::WhenIn { var: b, values: y, negated: false },
            ) => a == b && !x.iter().any(|v| y.contains(v)),
            (
                Verdict::RequiresDefined { var: a, negated: p },
                Verdict::RequiresDefined { var: b, negated: q },
            ) => a == b && p != q,
            _ => false,
        }
    }
}

/// Something provably wrong with a condition, independent of any variable's value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// `when: "{{ x == 1 }}"`. `when:` is templated implicitly; the delimiters cause
    /// double evaluation. Ansible deprecated this and it breaks on bare variables.
    JinjaDelimiters,
    /// `item` referenced with no `loop:`/`with_*` on the task — always undefined.
    ItemWithoutLoop,
    /// `x = 'y'` instead of `x == 'y'` — a Jinja syntax error at runtime.
    AssignmentNotComparison,
    /// Unbalanced `(`/`)` or an odd number of quotes — a Jinja syntax error.
    UnbalancedDelimiters,
    /// A clause that is a string stripping to empty. Refused outright since 2.19;
    /// silently True before it. Absence of a clause is not this — see [`problems`].
    EmptyCondition,
    /// The whole condition is a literal, so its result cannot be a boolean. Fatal since
    /// 2.19 ("Conditionals must have a boolean result"); silently truthy before it.
    NonBooleanLiteral,
}

/// How loudly a [`Problem`] should be reported. The strictness rules mean different things
/// on either side of 2.19, and the severity is where that difference lives — see
/// [`Problem::tier`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Error,
    Warning,
    Hint,
}

/// Conditionals became strict here: empty and non-boolean results turned fatal, and a
/// fully-wrapped condition became a resolve-then-evaluate with a deprecation attached.
const STRICT: Version = Version { major: 2, minor: 19, patch: 0 };

/// Whether the runtime we are describing refuses broken conditionals. An **undetected**
/// version is deliberately not strict: it lowers the strictness rules to a warning, which
/// is right on an old core and merely understated on a new one.
fn is_strict(core: Option<Version>) -> bool {
    matches!(core, Some(v) if v >= STRICT)
}

impl Problem {
    pub fn rule_id(&self) -> &'static str {
        match self {
            Problem::JinjaDelimiters => "when-jinja-delimiters",
            Problem::ItemWithoutLoop => "when-item-without-loop",
            Problem::AssignmentNotComparison => "when-assignment",
            Problem::UnbalancedDelimiters => "when-unbalanced",
            Problem::EmptyCondition => "when-empty",
            Problem::NonBooleanLiteral => "when-not-boolean",
        }
    }

    /// `core` is the detected ansible-core version, if any. The three rules 2.19 did not
    /// touch stay a warning whatever it says; only the strictness rules read it.
    pub fn tier(&self, core: Option<Version>) -> Tier {
        match self {
            Problem::EmptyCondition | Problem::NonBooleanLiteral => {
                if is_strict(core) {
                    Tier::Error
                } else {
                    Tier::Warning
                }
            }
            // A Jinja syntax error, which kills the task on every version — verified on both
            // 2.18.6 and 2.21.2, so there is nothing to gate. These read as errors because
            // that is what they are; shipping "raises a syntax error at runtime" as a warning
            // says the opposite of the message.
            Problem::AssignmentNotComparison | Problem::UnbalancedDelimiters => Tier::Error,
            // `item` undefined is equally fatal, but T-139 is a live false positive — the
            // loop can sit on the *including* task, which `problems` never sees, and three
            // corpus conditions hit exactly that. A warning until that is fixed.
            Problem::ItemWithoutLoop => Tier::Warning,
            // On 2.19+ this is no longer "evaluates twice": the value is resolved once and
            // then evaluated, and whether that deprecates depends on the runtime type of
            // the result. A hint can point at it; a warning would overstate it.
            Problem::JinjaDelimiters if is_strict(core) => Tier::Hint,
            Problem::JinjaDelimiters => Tier::Warning,
        }
    }

    /// `keyword` is the bare-expression keyword this fired on — `when`, `failed_when`,
    /// `changed_when`, `until`, or `that` under `assert:`. All five are the same expression
    /// language with the same failure modes, so naming the wrong one in a message is the
    /// difference between a fix and a hunt (T-141).
    pub fn message(&self, core: Option<Version>, keyword: &str) -> String {
        let strict = is_strict(core);
        match self {
            Problem::JinjaDelimiters if strict => format!(
                "`{keyword}:` is already a Jinja expression. This is resolved once and the \
                 result evaluated: if it resolves to a string it is an indirect expression, \
                 otherwise it is deprecated for removal in 2.23. Drop the delimiters and write \
                 the expression directly."
            ),
            Problem::JinjaDelimiters => format!(
                "`{keyword}:` is already a Jinja expression — `{{{{ }}}}` here evaluates twice \
                 and misbehaves on bare variables. Drop the delimiters."
            ),
            Problem::ItemWithoutLoop => format!(
                "`item` is only defined inside a loop, and this task has no `loop:`/`with_*` \
                 — this `{keyword}:` can never evaluate."
            ),
            Problem::AssignmentNotComparison => format!(
                "single `=` is assignment, not comparison — Jinja raises a syntax error here \
                 at runtime, failing the `{keyword}:`. Use `==`."
            ),
            Problem::UnbalancedDelimiters => format!(
                "unbalanced parentheses or quotes — Jinja raises a syntax error here at \
                 runtime, failing the `{keyword}:`."
            ),
            Problem::EmptyCondition => format!(
                "an empty `{keyword}:` {}. Remove it — an absent `{keyword}:` and \
                 `{keyword}: []` both mean \"no condition\" and are fine; only an empty string \
                 is refused.",
                Self::since_219(strict, core, "is refused outright", "evaluates as true")
            ),
            Problem::NonBooleanLiteral => format!(
                "this `{keyword}:` is a literal, so it cannot evaluate to a boolean, and a \
                 non-boolean result {}. Note that YAML quoting is invisible here: `\"x\"` is \
                 the variable `x`, while `\"'x'\"` is this literal.",
                Self::since_219(strict, core, "is an error", "is accepted as truthy")
            ),
        }
    }

    /// Both strictness messages need the same clause: what the runtime in front of the user
    /// does, and — when it is an old one — that upgrading changes the answer.
    fn since_219(strict: bool, core: Option<Version>, now: &str, before: &str) -> String {
        match (strict, core) {
            (true, Some(v)) => format!("{now} on the ansible-core {v} in use"),
            (true, None) => now.to_string(),
            (false, Some(v)) => {
                format!("{before} on the ansible-core {v} in use, and {now} from 2.19")
            }
            (false, None) => format!("{now} from ansible-core 2.19, and {before} before it"),
        }
    }
}

/// Provable faults in one condition. `has_loop` is whether the containing task carries
/// a `loop:`/`with_*`, which `item` depends on.
pub fn problems(cond: &str, has_loop: bool) -> Vec<Problem> {
    let mut out = Vec::new();
    // Stripped before the check upstream, so whitespace counts as empty. Nothing else can
    // be said about an empty string, so this is the whole verdict rather than one of many.
    if cond.trim().is_empty() {
        return vec![Problem::EmptyCondition];
    }
    // Only the *fully wrapped* form. A template embedded in a larger expression — inside a
    // string constant (`'{{ host }}' == x`) or beside one (`{{ a }}:{{ b }} in xs`) — is the
    // `ALLOW_EMBEDDED_TEMPLATES` case, which defaults on and emits no deprecation on 2.21.2;
    // diagnosing it would be louder than the runtime (T-117's audit, T-141's corpus run).
    if fully_wrapped(cond) || strip_strings(cond).contains("{%") {
        out.push(Problem::JinjaDelimiters);
    }
    if is_bare_literal(cond) {
        out.push(Problem::NonBooleanLiteral);
    }
    let bare = strip_strings(cond);
    if !has_loop && has_word(&bare, "item") {
        out.push(Problem::ItemWithoutLoop);
    }
    if lone_equals(&bare) {
        out.push(Problem::AssignmentNotComparison);
    }
    if bare.matches('(').count() != bare.matches(')').count() || unterminated_quote(cond) {
        out.push(Problem::UnbalancedDelimiters);
    }
    out
}

/// Each root variable *use* in a Jinja expression, with its byte range in `expr` — the
/// span-aware core of [`variables`]. Filters, tests, attribute accesses, magic vars and
/// string-literal contents are excluded; the root only (`foo.bar.baz` -> `foo`).
///
/// Scans the original text — not the string-stripped copy [`variables`] used to use — so
/// the offsets stay byte-accurate past non-ASCII. Uses are returned in order and NOT
/// deduplicated, so each occurrence keeps its own span.
pub fn variable_uses(expr: &str) -> Vec<(String, usize, usize)> {
    scan_words(expr, |w| {
        !(NOT_VARIABLES.contains(&w)
            || is_injected(w)
            || w.chars().next().is_some_and(|c| c.is_ascii_digit()))
    })
}

/// True for a name Ansible injects: a magic variable, or the `ansible_*` fact prefix.
/// [`variable_uses`] drops these — no rule can use a name no workspace file defines — and
/// [`any_uses`] keeps them, which is the difference between the rule view and hover's.
pub fn is_injected(name: &str) -> bool {
    MAGIC.contains(&name) || name.starts_with("ansible_")
}

/// [`variable_uses`] plus the injected names it drops. One scan answering "what name is
/// under this cursor", for a caller that will decide by [`is_injected`] which hover to
/// render — rather than two complementary scans of the same tree (T-143).
pub fn any_uses(expr: &str) -> Vec<(String, usize, usize)> {
    scan_words(expr, |w| {
        !(NOT_VARIABLES.contains(&w) || w.chars().next().is_some_and(|c| c.is_ascii_digit()))
    })
}

/// The shared tokenizer. `keep` decides what counts, so the two views above can never drift
/// on what a *word* is — only on which words they want.
fn scan_words(expr: &str, keep: impl Fn(&str) -> bool) -> Vec<(String, usize, usize)> {
    let bytes = expr.as_bytes();
    let mut out: Vec<(String, usize, usize)> = Vec::new();
    let mut i = 0;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        // Skip string-literal contents in place, so identifiers inside them aren't matched
        // and offsets outside them are unaffected.
        if let Some(q) = quote {
            if b == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if b == b'\'' || b == b'"' {
            quote = Some(b);
            i += 1;
            continue;
        }
        let c = b as char;
        if !(c.is_ascii_alphabetic() || c == '_') {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && {
            let c = bytes[i] as char;
            c.is_ascii_alphanumeric() || c == '_'
        } {
            i += 1;
        }
        let word = &expr[start..i];

        // A `(` after it makes it a call, not a variable.
        if expr[i..].trim_start().starts_with('(') {
            continue;
        }
        // Preceded by `.` -> an attribute. Preceded by `|` -> a filter name.
        let before = expr[..start].trim_end();
        if before.ends_with('.') || before.ends_with('|') {
            continue;
        }
        // `is defined` / `is not defined`: the test name, not a variable.
        if before.ends_with(" is") || before.ends_with(" is not") {
            continue;
        }
        if !keep(word) {
            continue;
        }
        out.push((word.to_string(), start, i));
    }
    out
}

/// Root variable names a condition depends on, with filters, tests, string literals,
/// attribute accesses and Ansible's magic variables removed.
///
/// The root only: `lustre_mount_check.stat.exists` yields `lustre_mount_check`, because
/// that's the name a workspace-wide definition search can actually match.
pub fn variables(cond: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (name, _, _) in variable_uses(cond) {
        if !out.iter().any(|v| v == &name) {
            out.push(name);
        }
    }
    out
}

/// Whether every clause guards itself, so an undefined variable is swallowed instead of
/// raising. This is what makes a typo permanent: `skip_smaba | default(false)` is false
/// forever and nothing ever complains.
pub fn is_guarded(conditions: &[String]) -> bool {
    !conditions.is_empty()
        && conditions
            .iter()
            .all(|c| c.contains("default(") || c.contains(" is defined") || c.contains(" is not defined"))
}

/// Combine the conditions on one task. A list `when:` is clauses ANDed together.
pub fn classify_all(conditions: &[String]) -> Verdict {
    let verdicts: Vec<_> = conditions.iter().map(|c| classify(c)).collect();
    if verdicts.contains(&Verdict::Never) {
        return Verdict::Never;
    }
    // Clauses are ANDed, so an `Always` clause constrains nothing and must not mask a
    // sibling that does. It only stands alone.
    if verdicts.iter().all(|v| *v == Verdict::Always) {
        return verdicts.into_iter().next().unwrap_or(Verdict::Unknown);
    }
    let unreadable = verdicts.iter().filter(|v| **v == Verdict::Unknown).count();
    let parts: Vec<Verdict> = verdicts
        .into_iter()
        .filter(|v| *v != Verdict::Unknown && *v != Verdict::Always)
        .collect();
    match (parts.len(), unreadable) {
        (0, _) => Verdict::Unknown,
        // A lone readable clause with unreadable siblings is NOT the whole condition,
        // so it must not be reported as though it were.
        (1, 0) => parts.into_iter().next().unwrap_or(Verdict::Unknown),
        _ => Verdict::All { parts, unreadable },
    }
}

pub fn classify(cond: &str) -> Verdict {
    let s = normalize(cond);
    if is_falsy(&s) {
        return Verdict::Never;
    }
    // `true` is in NOT_VARIABLES, so without this it falls through to `Unknown` and
    // `when: true` says nothing while `when: false` says "never runs".
    if is_truthy(&s) {
        return Verdict::Always;
    }
    if s.contains("{{") {
        // Double-templated; `problems()` reports it and the shape is unreliable.
        return Verdict::Unknown;
    }
    // Multiple clauses joined inline: no single summary, same rule as a list `when:`.
    if s.contains(" and ") || s.contains(" or ") {
        return Verdict::Unknown;
    }

    if let Some(inner) = strip_not(&s) {
        return match classify(&inner) {
            Verdict::OnlyIfSet { var } => Verdict::UnlessSet { var },
            Verdict::UnlessCleared { var } => Verdict::OnlyIfSet { var },
            Verdict::UnlessSet { var } => Verdict::OnlyIfSet { var },
            Verdict::WhenEquals { var, value, negated, matches_default } => Verdict::WhenEquals {
                var,
                value,
                negated: !negated,
                matches_default: !matches_default,
            },
            Verdict::WhenIn { var, values, negated } => Verdict::WhenIn {
                var,
                values,
                negated: !negated,
            },
            Verdict::RequiresDefined { var, negated } => Verdict::RequiresDefined {
                var,
                negated: !negated,
            },
            Verdict::Never => Verdict::Always,
            Verdict::Always => Verdict::Never,
            // De Morgan on a conjunction gives a disjunction, which these verdicts
            // cannot express. Refuse rather than invert it wrongly.
            Verdict::All { .. } => Verdict::Unknown,
            _ => Verdict::Unknown,
        };
    }

    // `x is defined` / `x is not defined`
    if let Some((lhs, rest)) = s.split_once(" is ") {
        let (negated, test) = match rest.strip_prefix("not ") {
            Some(t) => (true, t.trim()),
            None => (false, rest.trim()),
        };
        if test == "defined" {
            if let Some(var) = plain_var(lhs.trim()) {
                return Verdict::RequiresDefined { var, negated };
            }
        }
        return Verdict::Unknown;
    }

    // `x | default('') | length > 0`
    if let Some(lhs) = s.strip_suffix("> 0").map(str::trim) {
        if let Some(base) = lhs.strip_suffix("| length").map(str::trim) {
            if let Some(var) = parse_defaulted(base).map(|(v, _)| v).or_else(|| plain_var(base)) {
                return Verdict::RequiresNonEmpty { var };
            }
        }
        return Verdict::Unknown;
    }

    // `x in ['a', 'b']` / `x not in [...]`
    if let Some((lhs, rhs)) = split_membership(&s) {
        let (lhs, negated) = match lhs.strip_suffix(" not") {
            Some(l) => (l.trim(), true),
            None => (lhs, false),
        };
        if let Some(var) = parse_defaulted(lhs).map(|(v, _)| v).or_else(|| plain_var(lhs)) {
            let values = list_literals(rhs);
            if !values.is_empty() {
                return Verdict::WhenIn { var, values, negated };
            }
        }
        return Verdict::Unknown;
    }

    // `x | default('v') == 'lit'`, and the `!=` form
    for (op, negated) in [("==", false), ("!=", true)] {
        if let Some((lhs, rhs)) = s.rsplit_once(op) {
            let value = unquote(rhs.trim()).to_string();
            let lhs = strip_outer_parens(lhs);
            if let Some((var, dflt)) = parse_defaulted(lhs) {
                let matches_default = (unquote(&dflt) == value) != negated;
                return Verdict::WhenEquals { var, value, negated, matches_default };
            }
            // Unguarded `x == 'lit'`: no default, so nothing is known about an unset run.
            if plain_var(lhs).is_some() {
                return Verdict::Unknown;
            }
            return Verdict::Unknown;
        }
    }

    if let Some((var, dflt)) = parse_defaulted(&s) {
        if is_truthy(&dflt) {
            return Verdict::UnlessCleared { var };
        }
        if is_falsy(&dflt) {
            return Verdict::OnlyIfSet { var };
        }
    }

    Verdict::Unknown
}

fn normalize(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_strings(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut quote = None;
    for c in s.chars() {
        match quote {
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                out.push(' ');
            }
            None => out.push(c),
            Some(q) if c == q => {
                quote = None;
                out.push(' ');
            }
            Some(_) => out.push(' '),
        }
    }
    out
}

/// A quote that never closes. Counting each quote character's parity instead reports
/// `x == "it's"` as unbalanced, because the apostrophe inside the double-quoted string makes
/// the single-quote count odd — three such lines in the corpus, all correct Ansible (T-141).
/// Tracking which quote is open is the same walk [`strip_strings`] already does.
fn unterminated_quote(s: &str) -> bool {
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match quote {
            None if c == '\'' || c == '"' => quote = Some(c),
            Some(q) if c == q => quote = None,
            _ => {}
        }
    }
    quote.is_some()
}

/// The whole value is one `{{ … }}` and nothing else. Two templates back to back, or one
/// beside literal text, is embedded templating rather than the wrapped-expression case.
fn fully_wrapped(s: &str) -> bool {
    let t = s.trim();
    t.len() >= 4 && t.starts_with("{{") && t.ends_with("}}") && !t[2..t.len() - 2].contains("}}")
}

fn has_word(s: &str, word: &str) -> bool {
    s.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .any(|w| w == word)
}

/// A `=` that isn't part of `==`, `!=`, `<=`, `>=`, and isn't a Jinja keyword argument.
///
/// Runs on the output of [`strip_strings`], so a quoted `a=b` is already blanked out. That
/// stripping is also what makes the keyword-argument case hard: `map(attribute='path')`
/// arrives as `map(attribute=      )`, and the value that would have identified it is gone.
/// Two signals survive it (T-140):
///
/// - the `=` runs straight into a name, where an assignment is written `mode = 'x'`;
/// - it sits inside a **call**'s parentheses, which is the only place Jinja takes kwargs.
///
/// Both are required. `mode='docker'` outside parens is still an assignment, and so is
/// `(a = b)` inside grouping parens — a `(` counts as a call only when a name runs into it.
///
/// The name may be separated from the `=` by spaces: `map(attribute = "x")` is written that
/// way in kubespray and is a kwarg like any other, so the name is looked for past whitespace
/// (T-141's corpus run). The *operator* test still reads the byte immediately before, since
/// `==`/`!=`/`<=`/`>=` are never written with a gap.
fn lone_equals(s: &str) -> bool {
    let b = s.as_bytes();
    let name_char = |c: Option<u8>| matches!(c, Some(p) if p.is_ascii_alphanumeric() || p == b'_');
    // One entry per open paren: was it a call, `map(`, or a grouping, `(a or b)`? A keyword
    // like `not(` reads as a call here, which costs nothing — `not(a = b)` is not a shape
    // anyone writes, and treating it as grouping would need a keyword list.
    let mut calls: Vec<bool> = Vec::new();
    for (i, &c) in b.iter().enumerate() {
        let prev = i.checked_sub(1).map(|j| b[j]);
        match c {
            b'(' => {
                calls.push(name_char(prev));
                continue;
            }
            b')' => {
                calls.pop();
                continue;
            }
            b'=' => {}
            _ => continue,
        }
        let next = b.get(i + 1).copied();
        if matches!(prev, Some(b'=' | b'!' | b'<' | b'>' | b'~')) || next == Some(b'=') {
            continue;
        }
        let prev_name = b[..i].iter().rev().find(|c| !c.is_ascii_whitespace()).copied();
        if calls.last() == Some(&true) && name_char(prev_name) {
            continue;
        }
        return true;
    }
    false
}

fn strip_not(s: &str) -> Option<String> {
    let rest = s.strip_prefix("not ")?.trim();
    Some(normalize(strip_outer_parens(rest)))
}

/// ` in ` / ` not in ` at the top level, returning (lhs, rhs).
fn split_membership(s: &str) -> Option<(&str, &str)> {
    let at = s.find(" in ")?;
    Some((s[..at].trim(), s[at + 4..].trim()))
}

fn list_literals(s: &str) -> Vec<String> {
    let t = s.trim();
    // Must be an actual literal list. `groups['servers']` is a subscript, not a list,
    // and accepting it produced a bogus one-element "list" of `groups['servers'`.
    let inner = match (t.strip_prefix('['), t.strip_suffix(']')) {
        (Some(_), Some(_)) => &t[1..t.len() - 1],
        _ => match (t.strip_prefix('('), t.strip_suffix(')')) {
            (Some(_), Some(_)) => &t[1..t.len() - 1],
            _ => return Vec::new(),
        },
    };
    if inner.contains('[') || inner.contains('|') {
        return Vec::new();
    }
    inner
        .split(',')
        .map(|p| unquote(p.trim()).to_string())
        .filter(|p| !p.is_empty() && !p.contains(' '))
        .collect()
}

/// A bare variable name, with an optional dotted path. Returns the root.
fn plain_var(s: &str) -> Option<String> {
    let s = strip_outer_parens(s);
    let root = s.split('.').next()?.trim();
    if root.is_empty() || !root.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    if NOT_VARIABLES.contains(&root) {
        return None;
    }
    Some(root.to_string())
}

/// `x | default(false) | bool` -> `("x", "false")`. `None` unless the pipeline is a plain
/// variable followed by a `default(...)`, so anything with real logic falls through.
fn parse_defaulted(s: &str) -> Option<(String, String)> {
    let s = strip_outer_parens(s);
    let mut parts = s.split('|').map(str::trim);
    let var = plain_var(parts.next()?)?;
    let mut dflt = None;
    for p in parts {
        if let Some(arg) = p.strip_prefix("default(").and_then(|a| a.strip_suffix(')')) {
            dflt = Some(arg.trim().to_string());
        } else if p != "bool" {
            // An unrecognised filter could change the result; don't guess.
            return None;
        }
    }
    Some((var, dflt?))
}

fn unquote(s: &str) -> &str {
    let b = s.as_bytes();
    if s.len() >= 2 && (b[0] == b'\'' || b[0] == b'"') && b[0] == b[s.len() - 1] {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

fn is_truthy(s: &str) -> bool {
    matches!(unquote(s.trim()), "true" | "True" | "yes" | "1")
}

fn is_falsy(s: &str) -> bool {
    matches!(unquote(s.trim()), "false" | "False" | "no" | "0")
}

/// The whole condition is a Jinja literal, so no value of any variable can make its result a
/// boolean. Only unambiguous shapes count: `'a' == b` is built *from* a literal but is not
/// one, and a YAML boolean is the correct spelling of a constant condition rather than a
/// fault — `when: true` is demonstrated in the demo as `Verdict::Always`.
fn is_bare_literal(cond: &str) -> bool {
    let t = cond.trim();
    let b = t.as_bytes();
    if t.is_empty() || matches!(t, "true" | "True" | "false" | "False") {
        return false;
    }
    // A quoted string whose closing quote is the last character. The same quote appearing
    // in between means the value is an expression joining literals, not a single one.
    if b.len() >= 2 && (b[0] == b'\'' || b[0] == b'"') && b[b.len() - 1] == b[0] {
        return !t[1..t.len() - 1].contains(b[0] as char);
    }
    // A number — int or float, both non-boolean.
    if t.parse::<f64>().is_ok() {
        return true;
    }
    // A list or dict display. `{{ … }}` is a template, not a dict.
    (t.starts_with('[') && t.ends_with(']'))
        || (t.starts_with('{') && t.ends_with('}') && !t.starts_with("{{"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 45 of this repo's 56 import-level conditions are this shape.
    #[test]
    fn skip_flag_runs_unless_set() {
        let v = classify("not (skip_smtp | default(false) | bool)");
        assert_eq!(v, Verdict::UnlessSet { var: "skip_smtp".into() });
        assert_eq!(v.label().unwrap(), "runs unless skip_smtp is set");
    }

    #[test]
    fn skip_flag_without_parens_or_bool() {
        assert_eq!(
            classify("not skip_format | default(false)"),
            Verdict::UnlessSet { var: "skip_format".into() }
        );
    }

    /// The mutually-exclusive pair from playbooks/lustre-deploy-full.yml.
    #[test]
    fn equality_against_the_default_value() {
        let native = classify("lustre_deployment_mode | default('native') == 'native'");
        let docker = classify("lustre_deployment_mode | default('native') == 'docker'");
        assert_eq!(native.var(), Some("lustre_deployment_mode"));
        assert_eq!(native.label().unwrap(), "runs unless lustre_deployment_mode changes from native");
        assert_eq!(docker.label().unwrap(), "runs only if lustre_deployment_mode = docker");
        assert!(native.excludes(&docker));
        assert!(!native.excludes(&native));
    }

    #[test]
    fn not_equals_inverts() {
        let v = classify("mode | default('native') != 'native'");
        assert_eq!(v.label().unwrap(), "runs only if mode changes from native");
    }

    #[test]
    fn membership_lists() {
        let v = classify("ap_operation in ['snapshot-expose', 'snapshot-unexpose']");
        assert_eq!(
            v,
            Verdict::WhenIn {
                var: "ap_operation".into(),
                values: vec!["snapshot-expose".into(), "snapshot-unexpose".into()],
                negated: false,
            }
        );
        assert!(v.label().unwrap().starts_with("runs only if ap_operation is one of"));
        let neg = classify("mode not in ['a', 'b']");
        assert!(neg.label().unwrap().starts_with("runs unless mode is one of"));
        assert!(classify("mode in ['a']").excludes(&classify("mode in ['b']")));
    }

    #[test]
    fn is_defined_and_its_negation_exclude_each_other() {
        let d = classify("policies is defined");
        let u = classify("policies is not defined");
        assert_eq!(d, Verdict::RequiresDefined { var: "policies".into(), negated: false });
        assert_eq!(d.label().unwrap(), "runs only if policies is set");
        assert_eq!(u.label().unwrap(), "runs only if policies is unset");
        assert!(d.excludes(&u));
    }

    #[test]
    fn non_empty_check() {
        assert_eq!(
            classify("storage_hosts | default('') | length > 0"),
            Verdict::RequiresNonEmpty { var: "storage_hosts".into() }
        );
    }

    #[test]
    fn truthy_default_inverts() {
        assert_eq!(
            classify("enable_gui | default(true) | bool"),
            Verdict::UnlessCleared { var: "enable_gui".into() }
        );
        assert_eq!(
            classify("feature_x | default(false) | bool"),
            Verdict::OnlyIfSet { var: "feature_x".into() }
        );
    }

    /// `when: true` is a no-op, and saying so is the mirror of `when: false`.
    #[test]
    fn literal_true_always_runs() {
        assert_eq!(classify("true"), Verdict::Always);
        assert_eq!(classify("True"), Verdict::Always);
        // YAML 1.2 leaves `yes` a string; PyYAML would have made it a bool.
        assert_eq!(classify("yes"), Verdict::Always);
        assert_eq!(
            classify("true").label().unwrap(),
            "always runs — this `when:` has no effect"
        );
        assert_eq!(classify("not (true)"), Verdict::Never);
        assert_eq!(classify("not (false)"), Verdict::Always);
    }

    /// An `Always` clause in a list constrains nothing, so it must not hide a sibling
    /// that does.
    #[test]
    fn always_does_not_mask_an_informative_sibling() {
        assert_eq!(
            classify_all(&["true".into(), "not (skip_gui | default(false))".into()]),
            Verdict::UnlessSet { var: "skip_gui".into() }
        );
        assert_eq!(classify_all(&["true".into()]), Verdict::Always);
        assert_eq!(classify_all(&["true".into(), "true".into()]), Verdict::Always);
    }

    #[test]
    fn literal_false_never_runs() {
        assert_eq!(classify("false"), Verdict::Never);
        assert_eq!(classify("False"), Verdict::Never);
        assert_eq!(classify("false").label().unwrap(), "never runs");
    }

    /// End-to-end: a YAML boolean `when:` has to reach here as text. It used to arrive
    /// as "Boolean(false)", making `Never` unreachable for the only shape that yields it.
    #[test]
    fn a_yaml_boolean_when_reaches_the_classifier() {
        use crate::parse::Document;
        let doc = Document::new("- include_tasks: a.yml\n  when: false\n".to_string());
        let nodes = doc.parse().unwrap();
        let refs = crate::references::extract(&nodes);
        assert_eq!(refs[0].conditions, vec!["false".to_string()]);
        assert_eq!(classify_all(&refs[0].conditions), Verdict::Never);
    }

    /// Conditions with real logic in them. Refusing to answer is the feature.
    #[test]
    fn anything_with_real_logic_is_unknown() {
        for c in [
            "policies is defined and policies | length > 0",
            "ansible_facts['os_family'] == 'RedHat'",
            "inventory_hostname in groups['servers']",
            "result.rc != 0",
            "x | default(false) | some_unknown_filter",
            "'ftp' in (ec_container_expose | default(['http', 'ftp']))",
            "zfs_role == \"storage\"",
        ] {
            assert_eq!(classify(c), Verdict::Unknown, "should not classify: {c}");
            assert!(classify(c).label().is_none());
        }
    }

    /// Clauses in a list are ANDed. Reporting one of them as if it were the whole
    /// condition overstates it in one direction; reporting nothing understates it in
    /// the other. Both were wrong, so a multi-clause `when:` now says what it requires.
    #[test]
    fn multiple_clauses_are_reported_together() {
        // The case that prompted this: two readable clauses used to yield no hint.
        let both = classify_all(&[
            "not (skip_demo | default(false) | bool)".into(),
            "demo_mode | default('native') == 'docker'".into(),
        ]);
        assert_eq!(
            both.label().unwrap(),
            "runs only if skip_demo unset +1 more"
        );
        assert!(matches!(both, Verdict::All { unreadable: 0, .. }));
    }

    /// One readable clause plus an unreadable sibling is NOT the readable one — the task
    /// also needs whatever the other clause says.
    #[test]
    fn an_unreadable_sibling_is_counted_not_dropped() {
        let v = classify_all(&[
            "not (skip_gui | default(false) | bool)".into(),
            "result.rc != 0".into(),
        ]);
        assert_eq!(
            v.label().unwrap(),
            "runs only if skip_gui unset +1 more"
        );

        let two = classify_all(&[
            "not (skip_gui | default(false) | bool)".into(),
            "result.rc != 0".into(),
            "other.thing is match('x')".into(),
        ]);
        assert_eq!(
            two.label().unwrap(),
            "runs only if skip_gui unset +2 more"
        );

        // All clauses unreadable stays silent — there is nothing to say.
        assert_eq!(
            classify_all(&["result.rc != 0".into(), "a.b == c.d".into()]),
            Verdict::Unknown
        );
    }

    /// A single clause keeps the richer default-run wording; only ANDed clauses fall
    /// back to bare requirements.
    #[test]
    fn a_lone_clause_keeps_its_default_run_wording() {
        assert_eq!(
            classify_all(&["not (skip_gui | default(false) | bool)".into()]),
            Verdict::UnlessSet { var: "skip_gui".into() }
        );
        assert_eq!(
            classify_all(&["not (skip_gui | default(false) | bool)".into()])
                .label()
                .unwrap(),
            "runs unless skip_gui is set"
        );
    }

    #[test]
    fn every_requirement_shape_renders() {
        for (cond, req) in [
            ("demo_mode | default('native') != 'docker'", "demo_mode != docker"),
            ("proto in ['http', 'ftp']", "proto in [http, ftp]"),
            ("other not in ['a']", "other not in [a]"),
            ("flag is not defined", "flag unset"),
            ("hosts | default('') | length > 0", "hosts non-empty"),
            ("enabled | default(true) | bool", "enabled not false"),
        ] {
            assert_eq!(classify(cond).requirement().as_deref(), Some(req), "{cond}");
        }
    }

    #[test]
    fn multi_clause_names_the_first_and_counts_the_rest() {
        let v = classify_all(&[
            "demo_mode | default('native') != 'docker'".into(),
            "proto in ['http', 'ftp']".into(),
            "flag is not defined".into(),
        ]);
        assert_eq!(v.label().unwrap(), "runs only if demo_mode != docker +2 more");
    }

    #[test]
    fn never_and_always_still_dominate_correctly() {
        assert_eq!(classify_all(&["false".into(), "anything".into()]), Verdict::Never);
        // `true` constrains nothing, so it must not become a listed requirement.
        assert_eq!(
            classify_all(&["true".into(), "not (skip_gui | default(false))".into()]),
            Verdict::UnlessSet { var: "skip_gui".into() }
        );
    }

    // ---------------------------------------------------------------- problems

    #[test]
    fn jinja_delimiters_are_a_problem() {
        assert_eq!(
            problems("{{ foo == 'bar' }}", false),
            vec![Problem::JinjaDelimiters]
        );
        assert!(problems("foo == 'bar'", false).is_empty());
    }

    #[test]
    fn item_needs_a_loop() {
        assert_eq!(problems("item.rc == 0", false), vec![Problem::ItemWithoutLoop]);
        assert!(problems("item.rc == 0", true).is_empty());
        // The word inside a string literal is not a reference.
        assert!(problems("x == 'item'", false).is_empty());
        // Nor is it a substring of another identifier.
        assert!(problems("item_count > 0", false).is_empty());
    }

    #[test]
    fn assignment_is_not_comparison() {
        assert_eq!(
            problems("mode = 'docker'", false),
            vec![Problem::AssignmentNotComparison]
        );
        for ok in ["a == b", "a != b", "a >= 1", "a <= 1", "x is match('a=b')"] {
            assert!(
                !problems(ok, false).contains(&Problem::AssignmentNotComparison),
                "false positive on {ok}"
            );
        }
        // T-140 narrowed this rule; these are the shapes it must not have narrowed away.
        // A name running into `=` is a keyword argument only inside a *call* — grouping
        // parens and no parens at all are still assignments.
        for bad in ["mode = 'docker'", "mode='docker'", "(a = b)", "x and mode = 'y'"] {
            assert!(
                problems(bad, false).contains(&Problem::AssignmentNotComparison),
                "T-140 silenced a real assignment: {bad}"
            );
        }
    }

    /// T-117. An empty clause is refused since 2.19, and it is the *string* that decides:
    /// whitespace strips to empty and counts. Absence never reaches here — the parser makes a
    /// null `when:` a non-string, so it produces no clause at all.
    #[test]
    fn an_empty_condition_is_a_problem() {
        for c in ["", "   ", "\t", "\n "] {
            assert_eq!(problems(c, false), vec![Problem::EmptyCondition], "{c:?}");
        }
    }

    /// T-117. A literal cannot evaluate to a boolean, and 2.19 refuses a non-boolean result.
    /// Provable with no variable knowledge, which is what separates it from the rest of the
    /// boolean-result check.
    #[test]
    fn a_literal_condition_cannot_be_boolean() {
        for c in ["'bad'", "\"bad\"", "1", "0", "2.5", "[]", "[1, 2]", "{'a': 1}"] {
            assert!(problems(c, false).contains(&Problem::NonBooleanLiteral), "{c}");
        }
        // Built *from* literals is not the same as being one; a YAML boolean is the correct
        // spelling of a constant condition, not a fault; and a template is not a dict.
        for c in
            ["'a' == 'b'", "x == 'bad'", "true", "false", "x", "x | length > 0", "{{ x }}"]
        {
            assert!(!problems(c, false).contains(&Problem::NonBooleanLiteral), "{c}");
        }
    }

    /// T-141. Widening the rules to the other four keywords put 8097 real clauses through
    /// them instead of the `when:`-only subset, and these three shapes were reported as
    /// broken. All are correct Ansible, verbatim from the corpus, and each killed a different
    /// rule bug that `when:` alone never reached.
    #[test]
    fn shapes_the_corpus_proved_are_not_faults() {
        // An apostrophe inside a double-quoted string made the single-quote count odd.
        // `community.docker`, `docker_image/tasks/tests/options.yml:506`.
        let quoted = r#"labels_1.image.Config.Labels["this is a label"] == "this is the label's value""#;
        assert!(!problems(quoted, false).contains(&Problem::UnbalancedDelimiters), "{quoted}");

        // A kwarg written with spaces around the `=`. T-140 required the name to run into it.
        // kubespray, `tests/testcases/015_check-nodes-ready.yml:16`.
        let spaced = r#"x | map(attribute = "status.conditions") | list | min"#;
        assert!(!problems(spaced, false).contains(&Problem::AssignmentNotComparison), "{spaced}");

        // A template inside a string constant is embedded templating, which defaults to
        // allowed and warns about nothing. `community.crypto`, `get_certificate`.
        let embedded = "'{{ sni_host }}' == result.subject.CN";
        assert!(!problems(embedded, false).contains(&Problem::JinjaDelimiters), "{embedded}");
        // Nor is a template beside literal text. kubespray, `check_pull_required.yml:19`.
        let beside = "{{ download.repo }}:{{ download.tag }} in docker_images.stdout.split(',')";
        assert!(!problems(beside, false).contains(&Problem::JinjaDelimiters), "{beside}");

        // What survived the sweep, and should have: genuinely wrapped whole expressions.
        // kubespray, `validate_inventory/tasks/main.yml:73` and `assert-sorted-checksums.yml:18`.
        for c in [
            "{{ (kubelet_max_pods | default(110)) | int <= (2 ** (32 - x | int)) - 2 }}",
            "{{ item.1.value | reject('string') == [] }}",
        ] {
            assert!(problems(c, false).contains(&Problem::JinjaDelimiters), "{c}");
        }
    }

    /// T-117. The version picks the severity, never whether we speak. Measured upstream: on
    /// 2.18.6 an empty condition runs silently, on 2.21.2 it is fatal — so an old core earns a
    /// warning that it breaks on upgrade rather than the silence upstream gives it.
    #[test]
    fn strictness_severity_follows_the_detected_core() {
        let v = |minor, patch| Some(Version { major: 2, minor, patch });
        for p in [Problem::EmptyCondition, Problem::NonBooleanLiteral] {
            assert_eq!(p.tier(v(21, 2)), Tier::Error, "{p:?} on 2.21.2");
            assert_eq!(p.tier(v(19, 0)), Tier::Error, "{p:?} on 2.19.0, the boundary");
            assert_eq!(p.tier(v(18, 6)), Tier::Warning, "{p:?} on 2.18.6");
            assert_eq!(p.tier(None), Tier::Warning, "{p:?} undetected");
        }
        // The wrapped case is no longer "evaluates twice": on 2.19+ it is one of two outcomes
        // we cannot tell apart, so it drops to a hint and keeps its warning on an old core.
        assert_eq!(Problem::JinjaDelimiters.tier(v(21, 2)), Tier::Hint);
        assert_eq!(Problem::JinjaDelimiters.tier(v(18, 6)), Tier::Warning);
        // The three rules 2.19 did not touch never read the version.
        for p in [
            Problem::ItemWithoutLoop,
            Problem::AssignmentNotComparison,
            Problem::UnbalancedDelimiters,
        ] {
            assert_eq!(p.tier(v(21, 2)), p.tier(v(18, 6)), "{p:?} must not read the version");
            assert_eq!(p.tier(v(21, 2)), p.tier(None), "{p:?} must not read the version");
        }

        // A Jinja syntax error kills the task on every version — measured on 2.18.6 and
        // 2.21.2 — so it is an error, not a warning that contradicts its own message.
        assert_eq!(Problem::AssignmentNotComparison.tier(None), Tier::Error);
        assert_eq!(Problem::UnbalancedDelimiters.tier(None), Tier::Error);
        // `item` is just as fatal, and stays a warning only because T-139 makes it fire on
        // conditions that work — invert this when the including task's loop is visible.
        assert_eq!(Problem::ItemWithoutLoop.tier(None), Tier::Warning, "T-139 fixed? raise it");
    }

    /// A warning on an old core reads as "this is broken" unless it says otherwise — the point
    /// there is "this breaks when you upgrade", so the message has to name both runtimes.
    #[test]
    fn the_strictness_message_names_the_version_case() {
        let v18 = Some(Version { major: 2, minor: 18, patch: 6 });
        let v21 = Some(Version { major: 2, minor: 21, patch: 2 });

        let old = Problem::EmptyCondition.message(v18, "when");
        assert!(old.contains("2.18.6"), "{old}");
        assert!(old.contains("2.19"), "names the upgrade that changes it: {old}");

        let new = Problem::NonBooleanLiteral.message(v21, "when");
        assert!(new.contains("2.21.2"), "{new}");

        // An unchanged rule reads the same whatever the version.
        let p = Problem::AssignmentNotComparison;
        assert_eq!(p.message(None, "when"), p.message(v21, "when"));
    }

    /// T-141. The same fault in `failed_when:` must not be described as a `when:` problem —
    /// the reader has to find the line, and there may be several on one task.
    #[test]
    fn a_message_names_the_keyword_it_fired_on() {
        let v21 = Some(Version { major: 2, minor: 21, patch: 2 });
        for p in [
            Problem::JinjaDelimiters,
            Problem::ItemWithoutLoop,
            Problem::AssignmentNotComparison,
            Problem::UnbalancedDelimiters,
            Problem::EmptyCondition,
            Problem::NonBooleanLiteral,
        ] {
            for keyword in ["when", "failed_when", "changed_when", "until", "that"] {
                let m = p.message(v21, keyword);
                assert!(m.contains(&format!("`{keyword}:`")), "{p:?} on {keyword}: {m}");
            }
            // Naming one keyword must not leave another's name in the text.
            let m = p.message(v21, "failed_when");
            assert!(!m.contains("`when:`"), "{p:?} still says when: {m}");
        }
        // The rule ids stay `when-*` whatever the keyword, so existing `# noqa:` keeps working.
        assert_eq!(Problem::AssignmentNotComparison.rule_id(), "when-assignment");
    }

    #[test]
    fn unbalanced_delimiters() {
        assert!(problems("not (x | default(false)", false).contains(&Problem::UnbalancedDelimiters));
        assert!(problems("x == 'unterminated", false).contains(&Problem::UnbalancedDelimiters));
        assert!(!problems("not (x | default(false))", false).contains(&Problem::UnbalancedDelimiters));
    }

    // ---------------------------------------------------------------- variables

    #[test]
    fn extracts_root_variables_only() {
        assert_eq!(variables("lustre_mount_check.stat.exists"), vec!["lustre_mount_check"]);
        assert_eq!(
            variables("transport_mode | default('rdma') == 'rdma'"),
            vec!["transport_mode"]
        );
        assert_eq!(variables("snap_uuid is defined and snap_uuid | length > 0"), vec!["snap_uuid"]);
    }

    /// Every one of these fooled the first version of this extractor.
    #[test]
    fn ignores_literals_filters_tests_and_magic_vars() {
        // string literals
        assert!(variables("transport_mode | default('rdma') == 'tcp'").iter().all(|v| v == "transport_mode"));
        // bare filter names after a pipe
        assert_eq!(variables("x | default('') | length > 0"), vec!["x"]);
        assert!(!variables("y | bool").contains(&"bool".to_string()));
        // attribute access
        assert!(!variables("res.stdout_lines | length > 0").contains(&"stdout_lines".to_string()));
        // test names
        assert!(!variables("policies is defined").contains(&"defined".to_string()));
        assert!(!variables("res is not changed").contains(&"changed".to_string()));
        // magic and facts
        assert!(variables("inventory_hostname == 'x'").is_empty());
        assert!(variables("ansible_facts['os_family'] == 'RedHat'").is_empty());
        assert!(variables("item.key not in vars").is_empty());
    }

    /// What makes a typo permanent rather than loud.
    #[test]
    fn guarded_conditions_swallow_undefined_variables() {
        assert!(is_guarded(&["skip_x | default(false) | bool".into()]));
        assert!(is_guarded(&["snap_uuid is defined".into()]));
        assert!(!is_guarded(&["zfs_role == 'storage'".into()]));
        // Every clause must guard itself.
        assert!(!is_guarded(&[
            "skip_x | default(false)".into(),
            "zfs_role == 'storage'".into()
        ]));
        assert!(!is_guarded(&[]));
    }
}

/// Coverage against the real corpus. Not a unit test — a measurement, so it's ignored by
/// default. `cargo test -p ansible-core corpus -- --ignored --nocapture`
#[cfg(test)]
mod corpus {
    use super::*;
    use crate::parse::{Document, Node};

    /// Real `when:` expressions, verbatim, from four official Ansible collections. This is
    /// the corpus — it replaces a sweep over a private repo under `$HOME`, which skipped
    /// silently on every other machine and so proved nothing on CI (T-077).
    ///
    /// Harvested from these trees, pinned so the sample can be reproduced or widened:
    ///
    /// | `ansible-collections/…`  | commit    |
    /// | ------------------------ | --------- |
    /// | `ansible.posix`          | `ffdf9ef` |
    /// | `community.general`      | `6d6d64d` |
    /// | `community.docker`       | `bc8f6a6` |
    /// | `community.crypto`       | `498036f` |
    ///
    /// 1941 YAML files, 1211 `when:` sites, 1313 clauses, 416 distinct expressions. Below is
    /// the shape-diverse subset: every construct that appeared more than once, plus the
    /// awkward one-offs. YAML quoting is stripped, because that is what the parser hands us.
    ///
    /// These all ship and work upstream, so **any [`Problem`] reported here is a false
    /// positive** — that is the assertion these earn, and a synthetic fixture cannot make it.
    const REAL_WHENS: &[&str] = &[
        // -- community.crypto
        "cryptography_version is version('3.3', '>=')",
        "select_crypto_backend == 'cryptography'",
        "openssl_version is version('0.9.8zh', '>=')",
        "openssl_version is version('1.0.0', '>=')",
        "challenge_data is changed",
        "challenge_data is changed and challenge == 'http-01'",
        "challenge_data is changed and challenge in ['dns-01', 'dns-account-01']",
        "acme_roots[2].subject_key_identifier is defined",
        "acme_intermediates[0].subject_key_identifier is defined",
        "privatekey_fmt_2_step_1 is not failed",
        "create_passphrase_1 is failed",
        "backend == 'cryptography'",
        "cryptography_version is version('3.3', '>=') and bcrypt_version.stdout is version('3.1.5', '>=')",
        "test_keystore_path is defined",
        "has_java_keytool",
        // -- community.docker
        "docker_api_version is version('1.25', '>=')",
        "docker_py_version is version('2.6.0', '>=')",
        "docker_py_version is version('2.6.0', '<')",
        "docker_api_version is version('1.30', '>=') and docker_py_version is version('2.6.0', '>=')",
        "docker_api_version is version('1.28', '<') or docker_py_version is version('3.5.0', '<')",
        "not(docker_api_version is version('1.25', '>=')) and (ansible_facts.distribution != 'CentOS' or ansible_facts.distribution_major_version|int > 6)",
        "not remote_cert",
        "not docker_skip_cleanup",
        "needs_docker_daemon",
        "docker_has_buildx",
        "docker_has_compose and docker_compose_version is version('2.18.0', '>=')",
        "inspect is failed",
        "inspect is not failed",
        "docker_inspect is failed",
        "docker_inspect is not failed",
        "remove_all_images is failed",
        "registry_logs is not failed",
        "nginx_logs is not failed",
        // -- community.general
        "debug_test|default(false)|bool",
        "gitlab_premium_tests is defined",
        "credentials.username != ''",
        "credentials.username == ''",
        "ansible_facts.os_family == 'Debian'",
        "ansible_facts.os_family == 'Suse'",
        "ansible_facts.os_family == 'RedHat'",
        "ansible_facts.os_family != 'Darwin'",
        "ansible_facts.os_family == \"FreeBSD\"",
        "ansible_facts.distribution == 'Archlinux'",
        "ansible_facts.distribution == \"MacOSX\"",
        "ansible_facts.distribution == \"Ubuntu\"",
        "ansible_facts.distribution == \"RedHat\" and ansible_facts.distribution_major_version == \"8\"",
        "ansible_facts.os_family == \"RedHat\" and ansible_facts.distribution_major_version|int >= 7",
        "ansible_system == 'Linux'",
        "ansible_system == 'FreeBSD'",
        "ansible_system in ('FreeBSD', 'Linux')",
        "ansible_facts.system == 'Linux'",
        "ansible_facts == {}",
        "ansible_version.full is version('2.21', '>=')",
        "ansible_facts.python_version is version('3.8', '<')",
        "ansible_facts.distribution in ['Ubuntu', 'Debian']",
        "ansible_facts.os_family in ['Ubuntu', 'Debian']",
        "ansible_facts.distribution in ['MacOSX']",
        "has_snap",
        "has_gnupg",
        "has_hg is failed",
        "not sdkmanager_installed.stat.exists",
        "not in_check_mode",
        "yum_updates.results | length != 0",
        "updates.results | length > 0",
        "volume_info_all.storage_volumes | length > 0",
        "luks_extra_packages | length > 0",
        "terraform_version_installed is not defined or terraform_version_installed != terraform_version",
        "terraform_version_output.changed",
        "yum_versionlock_install is changed",
        "tty_1 is failed",
        "tty_1 is not failed",
        "storage_opts_1 is failed",
        "original_timezone is changed and original_timezone.diff.before.name != 'n/a'",
        "not with_alternatives and ansible_facts.os_family == 'RedHat'",
        "with_alternatives or ansible_facts.os_family != 'RedHat'",
        "lxml_xpath_attribute_result_attrname",
        "ansible_distribution in package_distros",
        "fstype == 'lvm'",
        "git_installed is succeeded and git_version.stdout is version(git_version_supporting_includes, \">=\")",
        "locale_basic.locales | intersect(initial_state.stdout_lines) != []",
        "url_removal_result is failed",
        "false",
        "ansible_facts.distribution_version is version('11.01', '>')",
        "ansible_facts.distribution == 'Fedora' and ansible_facts.distribution_major_version == '34'",
        "ansible_facts.os_family == 'RedHat' and ansible_facts.distribution != \"Fedora\" and (ansible_facts.distribution_major_version | int) >= 10",
        // -- ansible.posix
        "ansible_selinux is defined and ansible_selinux.status == 'disabled'",
        // -- kubespray. A deployment repo rather than a collection, so the shapes differ:
        // `group_names`/`inventory_hostname` inventory checks, parenthesised membership, and
        // `ansible_facts['x']` subscript rather than dotted access.
        "vxlan.stat.exists",
        "calico_rr_id is defined",
        "container_manager == \"docker\"",
        "container_manager ==  \"docker\"", // two spaces, exactly as upstream writes it
        "container_manager_on_localhost == 'crio'",
        "container_manager == 'containerd'",
        "calico_datastore == \"etcd\"",
        "etcd_deployment_type == \"kubeadm\"",
        "dns_mode in ['coredns', 'coredns_dual']",
        "dns_mode != 'none' and resolvconf_mode == 'docker_dns'",
        "('macvlan' not in testcase)",
        "('kube_control_plane' in group_names)",
        "('kube_control_plane' not in group_names)",
        "inventory_hostname == groups['kube_control_plane'] | last",
        "ansible_facts['distribution'] == \"Fedora\" and not is_ostree",
        "ansible_facts['os_family'] == \"RedHat\"",
        "external_openstack_region is not defined or not external_openstack_region",
        "docker_task_result is not changed",
        "etcd_ca_cert.changed and ansible_os_family == \"ClearLinux\"",
        "etcd_secret_changed | default(false)",
        "flush_iptables | bool and ipv4_stack",
        "enable_nat_default_gateway",
        "gateway_api_enabled",
        "drain_nodes",
        "fstab_file.stat.exists",
    ];

    /// Conditions we get **wrong**, the second kind: a Jinja *keyword argument* reads as an
    /// assignment once [`strip_strings`] has removed the quoted value, leaving `attribute=`
    /// or `operator=` and a lone `=`. All 12 `when-assignment` reports across kubespray were
    /// this, and none was a real fault. T-140.
    const JINJA_KWARG_NOT_ASSIGNMENT: &[&str] = &[
        "crio_version is version(\"1.29.0\", operator=\">=\")",
        "force_etcd_cert_refresh or not item in etcdcert_master.files | map(attribute='path') | list",
        "x | selectattr(\"path\", \"equalto\", p) | map(attribute=\"checksum\") | first",
    ];

    /// The list form, where every clause must hold. From `ansible.posix`'s selinux target
    /// (`tests/integration/targets/selinux/tasks/main.yml`) and `community.crypto`.
    const REAL_WHEN_LISTS: &[&[&str]] = &[
        &["ansible_selinux is defined", "ansible_selinux.status == 'enabled'"],
        &["select_crypto_backend == 'cryptography'", "cryptography_version is version('3.3', '>=')"],
    ];

    /// Conditions we get **wrong**. All three sit in task files that are `include_tasks`'d
    /// with the loop at the *include* site — `loop: "{{ cmd_echo_tests }}"` in
    /// `community.general`'s `cmd_runner/tasks/main.yml`, `with_sequence: start=1 end=2` in
    /// its `alternatives/tasks/tests.yml` — so `item` is defined and these run fine upstream.
    /// [`problems`] only sees the task's own mapping, so it calls all three broken. T-139.
    const ITEM_FROM_AN_INCLUDING_LOOP: &[&str] = &[
        "item.copy_to is defined",
        "ansible_facts.os_family != 'RedHat' or with_alternatives or item != 1",
        "ansible_facts.os_family == 'RedHat' and not with_alternatives and item == 1",
    ];

    fn whens(node: &Node, out: &mut Vec<(Vec<String>, bool)>) {
        match node {
            Node::Sequence { items, .. } => items.iter().for_each(|i| whens(i, out)),
            Node::Mapping { entries, .. } => {
                if let Some(w) = node.get("when") {
                    let has_loop = entries.iter().any(|(k, _)| {
                        matches!(k.as_str(), Some(s) if s == "loop" || s.starts_with("with_"))
                    });
                    let cs: Vec<String> = match w {
                        Node::Sequence { items, .. } => {
                            items.iter().filter_map(|i| i.as_str().map(str::to_owned)).collect()
                        }
                        o => o.as_str().map(str::to_owned).into_iter().collect(),
                    };
                    if !cs.is_empty() {
                        out.push((cs, has_loop));
                    }
                }
                entries.iter().for_each(|(_, v)| whens(v, out));
            }
            _ => {}
        }
    }

    /// The demo files are the only place these problems exist, so they double as the
    /// fixture. Not ignored — they must not silently stop demonstrating them.
    #[test]
    fn demo_exercises_every_problem_and_verdict() {
        let demo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../demo");
        let mut problems_found = Vec::new();
        let mut verdicts = Vec::new();
        for path in crate::workspace::yaml_files(&demo) {
            // `unparseable*.yml` are broken on purpose — the fixtures for the T-013 hint.
            if path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("unparseable")) {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("demo file");
            let doc = Document::new(text);
            let nodes = doc
                .parse()
                .unwrap_or_else(|| panic!("{} must stay parseable", path.display()));
            let mut ws = Vec::new();
            for n in &nodes {
                whens(n, &mut ws);
            }
            for (cs, has_loop) in ws {
                verdicts.push(classify_all(&cs));
                for c in &cs {
                    problems_found.extend(problems(c, has_loop));
                }
            }
        }

        for want in [
            Problem::JinjaDelimiters,
            Problem::ItemWithoutLoop,
            Problem::AssignmentNotComparison,
            Problem::UnbalancedDelimiters,
        ] {
            assert!(problems_found.contains(&want), "demo no longer shows {want:?}");
        }

        // Every verdict variant must be demonstrated, so a regression in any one of them
        // shows up as a demo that stopped explaining itself.
        let has = |f: &dyn Fn(&Verdict) -> bool| verdicts.iter().any(|v| f(v));
        assert!(has(&|v| matches!(v, Verdict::UnlessSet { .. })), "UnlessSet");
        assert!(has(&|v| matches!(v, Verdict::UnlessCleared { .. })), "UnlessCleared");
        assert!(has(&|v| matches!(v, Verdict::OnlyIfSet { .. })), "OnlyIfSet");
        assert!(has(&|v| matches!(v, Verdict::WhenIn { negated: false, .. })), "WhenIn");
        assert!(has(&|v| matches!(v, Verdict::WhenIn { negated: true, .. })), "WhenIn !");
        assert!(
            has(&|v| matches!(v, Verdict::RequiresDefined { negated: false, .. })),
            "RequiresDefined"
        );
        assert!(
            has(&|v| matches!(v, Verdict::RequiresDefined { negated: true, .. })),
            "RequiresDefined !"
        );
        assert!(has(&|v| matches!(v, Verdict::RequiresNonEmpty { .. })), "RequiresNonEmpty");
        assert!(has(&|v| *v == Verdict::Never), "Never");
        assert!(has(&|v| *v == Verdict::Always), "Always");
        assert!(has(&|v| *v == Verdict::Unknown), "Unknown");
        // Both polarities of the guarded comparison, incl. the `!=` form.
        assert!(
            has(&|v| matches!(v, Verdict::WhenEquals { matches_default: true, negated: false, .. })),
            "WhenEquals default-matches"
        );
        assert!(
            has(&|v| matches!(v, Verdict::WhenEquals { matches_default: false, negated: false, .. })),
            "WhenEquals default-differs"
        );
        assert!(
            has(&|v| matches!(v, Verdict::WhenEquals { negated: true, .. })),
            "WhenEquals negated"
        );
    }

    /// The assertion the corpus exists for. Every one of these ships and works upstream, so
    /// a [`Problem`] on any of them is us calling working Ansible broken — the P1 failure
    /// mode for a linter. Runs everywhere, unlike the `$HOME` sweep it replaces.
    #[test]
    fn no_shipped_condition_is_reported_as_broken() {
        let mut flagged = Vec::new();
        for c in REAL_WHENS {
            for p in problems(c, false) {
                flagged.push(format!("{}  <-  {c}", p.rule_id()));
            }
        }
        for cs in REAL_WHEN_LISTS {
            for c in *cs {
                for p in problems(c, false) {
                    flagged.push(format!("{}  <-  {c}", p.rule_id()));
                }
            }
        }
        assert!(
            flagged.is_empty(),
            "false positives on real, shipped conditions:\n  {}",
            flagged.join("\n  ")
        );
    }

    /// Nothing in the corpus may panic or hang the classifiers, whatever their verdict —
    /// the shapes here are wilder than anything written by hand (`~` concatenation,
    /// `regex_search` with backslashes, `intersect(...) != []`, indexed roots).
    #[test]
    fn every_real_condition_classifies_without_panicking() {
        for c in REAL_WHENS {
            let _ = classify(c);
            let _ = variables(c);
        }
        for cs in REAL_WHEN_LISTS {
            let cs: Vec<String> = cs.iter().map(|s| (*s).to_string()).collect();
            let _ = classify_all(&cs);
            let _ = is_guarded(&cs);
        }
    }

    /// A floor, not a target. The classifier deliberately answers `Unknown` for anything it
    /// cannot summarise honestly, so most of a real corpus is `Unknown` and that is correct.
    /// This pins the shapes it *does* claim, so a regression that quietly stops recognising
    /// `is defined` or `is version(...)` fails here instead of going unnoticed.
    #[test]
    fn real_world_coverage_does_not_regress() {
        let classified = REAL_WHENS.iter().filter(|c| classify(c) != Verdict::Unknown).count();
        // The guard forms specifically: these drive the "runs unless…" hover.
        let guarded = REAL_WHENS
            .iter()
            .filter(|c| is_guarded(std::slice::from_ref(&(*c).to_string())))
            .count();
        // 14/111 and 10/111 as measured. Low by design, and in line with the full sweep
        // (166/1313 across the collections, 221/2261 across kubespray) — the sample tracks
        // the corpus it came from rather than being cherry-picked for a flattering number.
        assert!(classified >= 14, "only {classified}/{} classified", REAL_WHENS.len());
        assert!(guarded >= 10, "only {guarded} guarded conditions recognised");
    }

    /// T-140, fixed. A Jinja keyword argument is not an assignment, and
    /// `map(attribute='path')` / `version(x, operator='>=')` are everywhere in real playbooks
    /// — these three are verbatim from kubespray, where all 12 `when-assignment` reports were
    /// this and none was a real fault.
    #[test]
    fn a_jinja_keyword_argument_is_not_an_assignment() {
        for c in JINJA_KWARG_NOT_ASSIGNMENT {
            assert!(
                !problems(c, true).contains(&Problem::AssignmentNotComparison),
                "false positive on {c}"
            );
        }
    }

    /// A live false positive, pinned so the fix has a test waiting. `item` **is** defined in
    /// these three: the loop sits on the `include_tasks` that pulls their file in, which
    /// [`problems`] never sees. Asserting the wrong answer on purpose — when T-139 lands,
    /// this test fails and gets inverted.
    #[test]
    fn item_from_an_including_loop_is_flagged_today_and_should_not_be() {
        for c in ITEM_FROM_AN_INCLUDING_LOOP {
            assert!(
                problems(c, false).contains(&Problem::ItemWithoutLoop),
                "{c}: T-139 fixed? invert this test"
            );
            // With the flag set — what a loop-aware caller would pass — they come out clean,
            // so the rule itself is right and only its input is missing.
            assert!(problems(c, true).is_empty(), "{c} is otherwise well-formed");
        }
    }
}
