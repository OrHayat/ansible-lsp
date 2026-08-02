//! What a `when:` says about a default run, and what's provably wrong with it.
//!
//! Not an evaluator — a matcher over a closed set of shapes, plus a variable extractor.
//! Of this repo's 3466 conditions a large fraction match no shape here and must stay
//! [`Verdict::Unknown`]; the moment this guesses, anything built on it becomes
//! untrustworthy, which is what got the call-hierarchy tree scrapped.
//!
//! The `| default(D)` filter is what makes the rest tractable: it states the value when
//! the variable is unset, so the condition carries its own default-run answer without
//! resolving anything. That matters — Ansible has 22 variable precedence levels.

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
}

impl Problem {
    pub fn rule_id(&self) -> &'static str {
        match self {
            Problem::JinjaDelimiters => "when-jinja-delimiters",
            Problem::ItemWithoutLoop => "when-item-without-loop",
            Problem::AssignmentNotComparison => "when-assignment",
            Problem::UnbalancedDelimiters => "when-unbalanced",
        }
    }

    pub fn message(&self) -> &'static str {
        match self {
            Problem::JinjaDelimiters => {
                "`when:` is already a Jinja expression — `{{ }}` here evaluates twice \
                 and misbehaves on bare variables. Drop the delimiters."
            }
            Problem::ItemWithoutLoop => {
                "`item` is only defined inside a loop, and this task has no \
                 `loop:`/`with_*` — the condition can never evaluate."
            }
            Problem::AssignmentNotComparison => {
                "single `=` is assignment, not comparison — Jinja raises a syntax \
                 error here at runtime. Use `==`."
            }
            Problem::UnbalancedDelimiters => {
                "unbalanced parentheses or quotes — Jinja raises a syntax error here \
                 at runtime."
            }
        }
    }
}

/// Provable faults in one condition. `has_loop` is whether the containing task carries
/// a `loop:`/`with_*`, which `item` depends on.
pub fn problems(cond: &str, has_loop: bool) -> Vec<Problem> {
    let mut out = Vec::new();
    if cond.contains("{{") || cond.contains("{%") {
        out.push(Problem::JinjaDelimiters);
    }
    let bare = strip_strings(cond);
    if !has_loop && has_word(&bare, "item") {
        out.push(Problem::ItemWithoutLoop);
    }
    if lone_equals(&bare) {
        out.push(Problem::AssignmentNotComparison);
    }
    if bare.matches('(').count() != bare.matches(')').count()
        || cond.matches('\'').count() % 2 == 1
        || cond.matches('"').count() % 2 == 1
    {
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
        if NOT_VARIABLES.contains(&word)
            || MAGIC.contains(&word)
            || word.starts_with("ansible_")
            || word.chars().next().is_some_and(|c| c.is_ascii_digit())
        {
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
    if verdicts.iter().any(|v| *v == Verdict::Never) {
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

fn has_word(s: &str, word: &str) -> bool {
    s.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .any(|w| w == word)
}

/// A `=` that isn't part of `==`, `!=`, `<=`, `>=`.
fn lone_equals(s: &str) -> bool {
    let b = s.as_bytes();
    for (i, &c) in b.iter().enumerate() {
        if c != b'=' {
            continue;
        }
        let prev = i.checked_sub(1).map(|j| b[j]);
        let next = b.get(i + 1).copied();
        if matches!(prev, Some(b'=' | b'!' | b'<' | b'>' | b'~')) || next == Some(b'=') {
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
    use crate::workspace::yaml_files;
    use std::path::PathBuf;

    fn repo() -> PathBuf {
        PathBuf::from(std::env::var("HOME").unwrap()).join("app/ansible")
    }

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

    #[test]
    #[ignore]
    fn when_coverage() {
        let root = repo();
        if !root.exists() {
            eprintln!("skip: {} absent", root.display());
            return;
        }
        let mut all = Vec::new();
        for p in yaml_files(&root) {
            let Ok(text) = std::fs::read_to_string(&p) else { continue };
            let doc = Document::new(text);
            let Some(nodes) = doc.parse() else { continue };
            for n in &nodes {
                whens(n, &mut all);
            }
        }
        let mut classified = 0;
        let mut clauses = 0;
        let mut clause_classified = 0;
        let mut probs = 0;
        let mut guarded = 0;
        for (cs, has_loop) in &all {
            if classify_all(cs) != Verdict::Unknown {
                classified += 1;
            }
            if is_guarded(cs) {
                guarded += 1;
            }
            for c in cs {
                clauses += 1;
                if classify(c) != Verdict::Unknown {
                    clause_classified += 1;
                }
                probs += problems(c, *has_loop).len();
            }
        }
        let pct = |n: usize, d: usize| if d == 0 { 0.0 } else { 100.0 * n as f64 / d as f64 };
        println!("\ntasks with when:      {}", all.len());
        println!("  classified          {classified} ({:.0}%)", pct(classified, all.len()));
        println!("  guarded by default  {guarded} ({:.0}%)", pct(guarded, all.len()));
        println!("individual clauses    {clauses}");
        println!("  classified          {clause_classified} ({:.0}%)", pct(clause_classified, clauses));
        println!("problems found        {probs}");
        assert_eq!(probs, 0, "repo is clean today; a nonzero count is a new find or a false positive");
    }
}
