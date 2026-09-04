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
use crate::jinja::{self, CmpOp, Const as JConst, Expr, ExprKind, UnOp};
use crate::parse::Node;

/// The six spellings jinja2's parser turns into a literal rather than a name.
///
/// Not a judgement call and not extensible: `parse_primary` decides it, and
/// `env.parse("{{ X }}")` answers `Const` for exactly these and `Name` for everything else —
/// `null`, `nil` and `TRUE` included, which really are variables and must still be checked.
///
/// Kept beside [`NOT_VARIABLES`] rather than inside it because it is a different kind of fact:
/// that list is a hand-maintained set of filter and test names, while this one is fixed by the
/// language. Writing the literals into that list is how `True`, `False` and `None` came to be
/// reported as undefined variables while their lowercase spellings were not — the same fact
/// written down twice, in `parser.rs` and here, and only one copy updated.
const LITERALS: &[&str] = &["true", "True", "false", "False", "none", "None"];

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
    "not", "and", "or", "in", "is", "if", "else", "defined",
    "undefined", "changed", "failed", "succeeded", "success", "skipped", "match",
    "search", "version", "subset", "superset", "iterable", "mapping", "sequence",
    "number", "boolean", "even", "odd", "sameas", "escaped", "truthy", "falsy",
];


/// A variable reference in a condition: the name that is bound, and the accessor path
/// applied to it. They part company the moment there is an accessor — `r.stdout` binds `r`,
/// but what the condition is a statement *about* is `r.stdout` — and the two halves go to
/// different consumers. A definition lookup, hover target or provenance walk resolves the
/// root and has no use for the accessor; a label or a requirement must render the whole
/// path, because that is what the condition said.
///
/// Collapsing both into one `String` is T-186: only the root fit, so the hint on
/// `r.stdout | length > 0` read "runs only if r is non-empty". A registered result is a dict
/// carrying `changed`, `rc` and friends, so `r` is never empty — the hint asserted the task
/// would run in exactly the cases it is skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarRef {
    root: String,
    expr: String,
}

impl VarRef {
    /// The name a lookup resolves. Never the accessor path — nothing can look that up.
    pub fn root(&self) -> &str {
        &self.root
    }

    /// The reference as written, which is the only thing a label may make a claim about.
    pub fn expr(&self) -> &str {
        &self.expr
    }
}

/// Renders the expression, so every `{var}` in a label or requirement names the whole path
/// without each call site having to remember which half it wanted.
impl std::fmt::Display for VarRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.expr)
    }
}

/// Test-only, so a fabricated root cannot reach production: an expectation is built by the
/// same reader production uses, so a shape `classify` would refuse cannot be asserted here.
/// Panics rather than inventing a root, so a typo in an expectation fails instead of quietly
/// asserting itself.
#[cfg(test)]
impl From<&str> for VarRef {
    fn from(s: &str) -> Self {
        jinja::parse(s)
            .ok()
            .and_then(|e| var_ref(s, &e))
            .unwrap_or_else(|| panic!("not a variable reference: {s}"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// `not (skip_x | default(false) | bool)` — 80% of import-level conditions here.
    UnlessSet { var: VarRef },
    /// `x | default(true) | bool`
    UnlessCleared { var: VarRef },
    /// `x | default(false) | bool` — the flag has to be turned on.
    OnlyIfSet { var: VarRef },
    /// `mode | default('native') == 'native'`. `matches_default` is whether the
    /// defaulted value satisfies the comparison, i.e. whether this runs when unset.
    WhenEquals {
        var: VarRef,
        value: String,
        negated: bool,
        matches_default: bool,
    },
    /// `mode in ['a', 'b']`. `matches_default` is whether the *defaulted* value satisfies the
    /// membership, i.e. whether this runs when the variable is unset — `Some(false)` for a
    /// guard whose default does not satisfy it, and `None` when there is no guard at all.
    ///
    /// The two are not interchangeable and that is why this is not a `bool`. Unguarded, an
    /// unset variable is a **fatal error rather than a skip** (measured on 2.21.2), so neither
    /// this verdict nor its inverse runs by default — `None` must stay `None` through
    /// [`invert`], where `Some(false)` becomes `Some(true)`.
    WhenIn {
        var: VarRef,
        values: Vec<String>,
        negated: bool,
        matches_default: Option<bool>,
    },
    /// `x is defined` / `x is not defined`. Statically this is the interesting one:
    /// if `x` is defined nowhere in the workspace, the branch can never be taken.
    RequiresDefined { var: VarRef, negated: bool },
    /// `x | default('') | length > 0`
    RequiresNonEmpty { var: VarRef },
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
            Verdict::WhenIn { var, values, negated, matches_default } => {
                let list = values.join(", ");
                // `matches_default` picks the framing and `negated` picks the direction, the
                // same split [`Verdict::WhenEquals`] uses. "Runs unless" is this module's
                // wording for *runs by default*, so it is only ever correct for `Some(true)`
                // — T-213 was that phrase on a condition that does not run by default.
                match (matches_default, negated) {
                    (Some(true), false) => format!("runs unless {var} leaves [{list}]"),
                    (Some(true), true) => format!("runs unless {var} is one of [{list}]"),
                    (_, false) => format!("runs only if {var} is one of [{list}]"),
                    (_, true) => format!("runs only if {var} is not one of [{list}]"),
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
            Verdict::WhenIn { var, values, negated, .. } => format!(
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

    /// The variable this verdict hinges on, if it hinges on exactly one. The **root**, so
    /// the result is a name something can resolve — `r.stdout` hinges on `r`. What the
    /// verdict is a statement about is the whole path, which is what [`Verdict::label`]
    /// renders; the two are deliberately not the same string (T-186).
    pub fn var(&self) -> Option<&str> {
        match self {
            Verdict::UnlessSet { var }
            | Verdict::UnlessCleared { var }
            | Verdict::OnlyIfSet { var }
            | Verdict::WhenEquals { var, .. }
            | Verdict::WhenIn { var, .. }
            | Verdict::RequiresDefined { var, .. }
            | Verdict::RequiresNonEmpty { var } => Some(var.root()),
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
                Verdict::WhenIn { var: a, values: x, negated: false, .. },
                Verdict::WhenIn { var: b, values: y, negated: false, .. },
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
/// span-aware core of [`variables`]. Filters, tests, attribute accesses, names ansible
/// provides and string-literal contents are excluded; the root only (`foo.bar.baz` -> `foo`).
///
/// Scans the original text — not the string-stripped copy [`variables`] used to use — so
/// the offsets stay byte-accurate past non-ASCII. Uses are returned in order and NOT
/// deduplicated, so each occurrence keeps its own span.
pub fn variable_uses(expr: &str) -> Vec<(String, usize, usize)> {
    scan_words(expr, |w| {
        !(LITERALS.contains(&w)
            || NOT_VARIABLES.contains(&w)
            || crate::injected::provided(w)
            || w.chars().next().is_some_and(|c| c.is_ascii_digit()))
    })
}

/// [`variable_uses`] plus the names ansible provides ([`crate::injected::provided`]), which
/// it drops. One scan answering "what name is under this cursor", for a caller that looks
/// for a definition first and falls back to the injected table — rather than two
/// complementary scans of the same tree (T-143, T-224).
pub fn any_uses(expr: &str) -> Vec<(String, usize, usize)> {
    scan_words(expr, |w| {
        !(LITERALS.contains(&w)
            || NOT_VARIABLES.contains(&w)
            || w.chars().next().is_some_and(|c| c.is_ascii_digit()))
    })
}

/// The variable a `hostvars[...]` lookup reads, with its byte range in `expr` (T-104).
///
/// [`variable_uses`] cannot see these: it takes the *root* of an expression, and in
/// `hostvars['web01'].app_port` the root is `hostvars` — an injected name it drops — while
/// `app_port` is an attribute it skips. So the name that actually matters produces no use
/// at all, which is why neither hover nor any rule could say a word about it.
///
/// Both spellings of the read are recognised, and only those: `.name` and `['name']`. A
/// dynamic second subscript (`hostvars[h][var]`) names nothing statically and is skipped
/// rather than guessed. The host key itself is left to [`variable_uses`], which already
/// reports a variable used there.
pub fn hostvars_uses(expr: &str) -> Vec<(String, usize, usize)> {
    let bytes = expr.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
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
        if !(b as char).is_ascii_alphabetic() && b != b'_' {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && ((bytes[i] as char).is_ascii_alphanumeric() || bytes[i] == b'_') {
            i += 1;
        }
        // `x.hostvars` is somebody else's attribute, not the magic dict.
        if &expr[start..i] != "hostvars" || expr[..start].trim_end().ends_with('.') {
            continue;
        }
        let Some(after_key) = past_subscript(expr, i) else { continue };
        match read_name(expr, after_key) {
            Some(hit) => {
                i = hit.2;
                out.push(hit);
            }
            None => i = after_key,
        }
    }
    out
}

/// The *host* named by a `hostvars['...']` subscript containing byte `at`, with its span
/// (T-171). Only a literal key: `hostvars[some_var]` names no host we can know, and the
/// variable in it is already reported by [`variable_uses`].
///
/// `text` is the whole document rather than one expression, since the caller has a cursor
/// byte and not an expression — the scan finds the enclosing subscript itself.
pub fn hostvars_host_key_at(text: &str, at: usize) -> Option<(String, usize, usize)> {
    hostvars_host_keys(text)
        .into_iter()
        .find(|(_, s, e)| at >= *s && at <= *e)
}

/// Every literal `hostvars['...']` host key in `text`, with spans. The whole-document form
/// is what paints the links; [`hostvars_host_key_at`] is the same scan filtered to a cursor,
/// so what is clickable and what is painted cannot disagree.
pub fn hostvars_host_keys(text: &str) -> Vec<(String, usize, usize)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel) = text[from..].find("hostvars") {
        let start = from + rel;
        from = start + "hostvars".len();
        let before_ok = text[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_' && c != '.');
        if !before_ok {
            continue;
        }
        let Some(end) = past_subscript(text, from) else { continue };
        // The key is the literal between the brackets, quotes excluded.
        let inner = &text[from..end];
        let Some(open) = inner.find(['\'', '"']) else { continue };
        let q = inner.as_bytes()[open];
        let rest = &inner[open + 1..];
        let Some(len) = rest.find(q as char) else { continue };
        let (s, e) = (from + open + 1, from + open + 1 + len);
        out.push((text[s..e].to_string(), s, e));
        from = end;
    }
    out
}

/// The [`hostvars_host_keys`] that sit in a **value** the parser kept, dropping any that
/// live in a `#` comment.
///
/// A separate entry point rather than a fix to the scan, because the two callers are asking
/// different questions. Painting a clickable link over a host name in a comment is helpful
/// and costs nothing if it is wrong; reporting an ERROR on one is a false positive on a line
/// that never runs. Measured need, not a hypothetical: of the 23 literal `hostvars['...']`
/// uses in the reference corpus, 2 are inside comments — both in a role's prose explaining an
/// inventory schema, neither a use at all.
///
/// Containment in a scalar's span is the test, rather than hunting `#` in the text: the key
/// almost always sits inside a quoted Jinja string, where a textual comment scan has to
/// re-decide what YAML already decided.
pub fn hostvars_host_uses(text: &str, nodes: &[Node]) -> Vec<(String, usize, usize)> {
    fn covers(node: &Node, at: usize) -> bool {
        match node {
            Node::Scalar { span, .. } => at >= span.start && at < span.end,
            Node::Sequence { items, .. } => items.iter().any(|i| covers(i, at)),
            Node::Mapping { entries, .. } => {
                entries.iter().any(|(k, v)| covers(k, at) || covers(v, at))
            }
            _ => false,
        }
    }
    hostvars_host_keys(text)
        .into_iter()
        .filter(|(_, s, e)| is_only_literal(text, *s, *e))
        .filter(|(_, s, e)| !expression_swallows_undefined(text, *s, *e))
        .filter(|(_, s, _)| nodes.iter().any(|n| covers(n, *s)))
        .collect()
}

/// The names `hostvars` answers for whether or not an inventory lists them.
///
/// Ansible's implicit localhost is reachable under all three spellings — measured on 2.21.2
/// against an inventory containing only `web01`, every one of these is a member *and*
/// resolves (`ansible_connection` comes back `local`). Escaping only `localhost`, which is
/// what this rule shipped with first, leaves the other two as false errors.
pub const IMPLICIT_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "::1"];

/// Does the `{{ … }}` expression around this span swallow an undefined value?
///
/// `hostvars['web0143'].x` is fatal; `hostvars['web0143'].x | default('nope')` prints
/// `nope` — measured in the same run. So the subscript alone does not decide whether
/// anything fails, and a rule that says "this fails at runtime" has to look at the
/// expression it sits in.
///
/// The enclosing `{{ … }}` is the unit, not the whole scalar: in
/// `"{{ hostvars['typo'].x }} {{ y | default(1) }}"` the default belongs to the second
/// expression and rescues nothing. Within that unit the test is the blunt substring one
/// [`is_guarded`] already uses for `when:` clauses, and blunt in the safe direction — a
/// spurious match costs a missed report, never a false error.
fn expression_swallows_undefined(text: &str, s: usize, e: usize) -> bool {
    let open = text[..s].rfind("{{").or_else(|| text[..s].rfind("{%"));
    let close = text[e..].find("}}").or_else(|| text[e..].find("%}")).map(|i| e + i);
    let (Some(open), Some(close)) = (open, close) else {
        return false;
    };
    let expr = &text[open..close];
    expr.contains("default(") || expr.contains(" is defined") || expr.contains(" is not defined")
}

/// Is the subscript *nothing but* this quoted string — `hostvars['web01']` and not
/// `hostvars[groups['web'][0]]`?
///
/// [`hostvars_host_keys`] takes the first quoted run inside the balanced brackets, which for
/// a nested lookup is the inner expression's argument. That is a **group** name, and a group
/// is not a host, so treating it as one reports a typo in correct code. Measured against the
/// reference corpus: 20 of the 21 hits this rule first produced were this exact shape,
/// `hostvars[groups['app_servers'][0]]` — the idiomatic "first host of a group", every one
/// of them working code.
///
/// Checked here rather than in the scan for the same reason the comment filter is: the
/// link-painting caller wants whatever host name it can find, and an ERROR needs the name to
/// be the whole of what was written.
fn is_only_literal(text: &str, s: usize, e: usize) -> bool {
    let (Some(before), Some(after)) = (text.get(..s.saturating_sub(1)), text.get(e + 1..)) else {
        return false;
    };
    // `hostvars[` and not merely `[`: the inner bracket of `hostvars[groups['x'][0]]` ends
    // with one too, which is how the first version of this passed every case it existed to
    // reject.
    before.trim_end().ends_with("hostvars[") && after.trim_start().starts_with(']')
}

/// One byte past the balanced `[...]` beginning at or after `at`, skipping string
/// literals so a `]` inside the host name cannot close it early.
fn past_subscript(expr: &str, at: usize) -> Option<usize> {
    let bytes = expr.as_bytes();
    let mut i = at;
    while i < bytes.len() && (bytes[i] as char).is_whitespace() {
        i += 1;
    }
    if *bytes.get(i)? != b'[' {
        return None;
    }
    i += 1;
    let mut depth = 1usize;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'\'' || b == b'"' => quote = Some(b),
            None if b == b'[' => depth += 1,
            None if b == b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            None => {}
        }
        i += 1;
    }
    None
}

/// The name read off the host's vars — `.app_port` or `['app_port']` — as an identifier.
fn read_name(expr: &str, at: usize) -> Option<(String, usize, usize)> {
    let bytes = expr.as_bytes();
    let mut i = at;
    while i < bytes.len() && (bytes[i] as char).is_whitespace() {
        i += 1;
    }
    let (s, e) = match *bytes.get(i)? {
        b'.' => {
            i += 1;
            let s = i;
            while i < bytes.len()
                && ((bytes[i] as char).is_ascii_alphanumeric() || bytes[i] == b'_')
            {
                i += 1;
            }
            (s, i)
        }
        b'[' => {
            i += 1;
            while i < bytes.len() && (bytes[i] as char).is_whitespace() {
                i += 1;
            }
            let q = *bytes.get(i)?;
            if q != b'\'' && q != b'"' {
                // `hostvars[h][var]` — the name is itself a variable, unknowable here.
                return None;
            }
            i += 1;
            let s = i;
            while i < bytes.len() && bytes[i] != q {
                i += 1;
            }
            if i >= bytes.len() {
                return None;
            }
            (s, i)
        }
        _ => return None,
    };
    let name = &expr[s..e];
    let ident = !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    ident.then(|| (name.to_string(), s, e))
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

/// What a condition says, read from its parse tree.
///
/// Every arm below used to be a `strip_suffix` or a `split_once` against one exact spelling
/// of a construct Jinja accepts in many (T-188). Reading the tree is not a tidier way to do
/// the same thing — it is what makes `hosts|length>0` and `hosts | length > 0` the same
/// question (T-187), and what stops `r.stdout | length > 0` becoming a claim about `r`
/// (T-186), because the parser has already decided that a filter wraps the accessor path
/// rather than its root.
///
/// A condition that does not parse is [`Verdict::Unknown`], never a guess. `problems()` is
/// what reports *why* it does not parse; this only declines to describe it.
pub fn classify(cond: &str) -> Verdict {
    // Double-templated. `problems()` reports it, and Ansible evaluates the result of the
    // inner render, so the shape here says nothing about the condition that actually runs.
    if cond.contains("{{") {
        return Verdict::Unknown;
    }
    match jinja::parse(cond) {
        Ok(e) => classify_expr(cond, &e),
        Err(_) => Verdict::Unknown,
    }
}

fn classify_expr(src: &str, e: &Expr) -> Verdict {
    // A constant condition, in any of the spellings Ansible's YAML can deliver. `when: yes`
    // reaches here as a bare name because Jinja has no such keyword; `when: 1` as an integer.
    if let Some(b) = constant_truth(e) {
        return if b { Verdict::Always } else { Verdict::Never };
    }

    match &e.kind {
        // De Morgan on a conjunction gives a disjunction, which these verdicts cannot
        // express, so `Verdict::All` inverts to `Unknown` rather than wrongly.
        ExprKind::Unary { op: UnOp::Not, node } => invert(classify_expr(src, node)),

        // `x is defined` / `x is not defined` — the negated spelling arrives as a `Not`
        // around this, so it is handled by the arm above.
        ExprKind::Test { node, name, args } if name == "defined" && args.is_empty() => {
            match var_ref(src, node) {
                Some(var) => Verdict::RequiresDefined { var, negated: false },
                None => Verdict::Unknown,
            }
        }

        ExprKind::Compare { expr, ops } if ops.len() == 1 => {
            let (op, rhs) = &ops[0];
            match op {
                // `x | default('') | length > 0`
                // `length` must be the *only* filter this module does not model. Any other
                // — `sort`, `select`, `unique` — can change what is being counted, so
                // passing it through would be a guess about a different value.
                CmpOp::Gt if is_zero(rhs) => match strip_guards(src, expr) {
                    Some((var, _, filters)) if filters == ["length"] => {
                        Verdict::RequiresNonEmpty { var }
                    }
                    _ => Verdict::Unknown,
                },
                // `mode | default('native') == 'native'`, and the `!=` form.
                CmpOp::Eq | CmpOp::Ne => {
                    let negated = matches!(op, CmpOp::Ne);
                    let Some(value) = literal_text(rhs) else { return Verdict::Unknown };
                    match strip_guards(src, expr) {
                        // Unguarded `x == 'lit'`: no default, so nothing is known about an
                        // unset run, and a verdict would be inventing one.
                        Some((var, Some(dflt), filters)) if filters.is_empty() => {
                            let matches_default = (dflt == value) != negated;
                            Verdict::WhenEquals { var, value, negated, matches_default }
                        }
                        _ => Verdict::Unknown,
                    }
                }
                // `mode in ['a', 'b']` / `mode not in [...]`
                CmpOp::In | CmpOp::NotIn => {
                    let negated = matches!(op, CmpOp::NotIn);
                    let Some((var, dflt, filters)) = strip_guards(src, expr) else {
                        return Verdict::Unknown;
                    };
                    if !filters.is_empty() {
                        return Verdict::Unknown;
                    }
                    let values = literal_list(rhs);
                    if values.is_empty() {
                        return Verdict::Unknown;
                    }
                    let matches_default = dflt.map(|d| values.contains(&d) != negated);
                    Verdict::WhenIn { var, values, negated, matches_default }
                }
                _ => Verdict::Unknown,
            }
        }

        // `not (skip_x | default(false) | bool)` reaches here through the `Not` arm, so this
        // is the bare `x | default(D) | bool` form.
        ExprKind::Filter { .. } => match strip_guards(src, e) {
            Some((var, Some(dflt), filters)) if filters.is_empty() => {
                if is_truthy(&dflt) {
                    Verdict::UnlessCleared { var }
                } else if is_falsy(&dflt) {
                    Verdict::OnlyIfSet { var }
                } else {
                    Verdict::Unknown
                }
            }
            _ => Verdict::Unknown,
        },

        _ => Verdict::Unknown,
    }
}

/// The inverse of a verdict, for `not (...)`.
fn invert(v: Verdict) -> Verdict {
    match v {
        Verdict::OnlyIfSet { var } => Verdict::UnlessSet { var },
        Verdict::UnlessCleared { var } => Verdict::OnlyIfSet { var },
        Verdict::UnlessSet { var } => Verdict::OnlyIfSet { var },
        Verdict::WhenEquals { var, value, negated, matches_default } => Verdict::WhenEquals {
            var,
            value,
            negated: !negated,
            matches_default: !matches_default,
        },
        Verdict::WhenIn { var, values, negated, matches_default } => Verdict::WhenIn {
            var,
            values,
            negated: !negated,
            // `None` is "unset is an error", which the inverse does not change.
            matches_default: matches_default.map(|m| !m),
        },
        Verdict::RequiresDefined { var, negated } => {
            Verdict::RequiresDefined { var, negated: !negated }
        }
        Verdict::Never => Verdict::Always,
        Verdict::Always => Verdict::Never,
        // De Morgan turns a conjunction into a disjunction, which `All` cannot express.
        _ => Verdict::Unknown,
    }
}

/// Whether the whole expression is a constant, and which way.
///
/// Wider than Jinja's own `true`/`false`, because Ansible's YAML hands us `yes`, `no`, `1`
/// and `0` — and a quoted `'true'` is still a constant condition, not a variable.
fn constant_truth(e: &Expr) -> Option<bool> {
    let word = match &e.kind {
        ExprKind::Const(JConst::Bool(b)) => return Some(*b),
        ExprKind::Const(JConst::Str(s)) => s.as_str(),
        // A bare `yes`/`no` is a name to Jinja: it has no such keyword.
        ExprKind::Name(n) => n.as_str(),
        ExprKind::Const(JConst::Int(0)) => return Some(false),
        ExprKind::Const(JConst::Int(1)) => return Some(true),
        _ => return None,
    };
    if is_truthy(word) {
        Some(true)
    } else if is_falsy(word) {
        Some(false)
    } else {
        None
    }
}

fn is_zero(e: &Expr) -> bool {
    matches!(e.kind, ExprKind::Const(JConst::Int(0)))
}

/// Peel the guard filters off a reference, returning the reference, the `default(...)`
/// argument if there was one, and any filters left over that this module does not model.
///
/// `default` and `d` are the same filter — ansible-core registers both
/// (`plugins/filter/core.py`), and T-211 is the bug where only the long spelling classified.
/// `bool` is passed through because it changes nothing about which value is used when the
/// variable is unset, which is the only question these verdicts answer.
fn strip_guards<'a>(src: &str, e: &'a Expr) -> Option<(VarRef, Option<String>, Vec<String>)> {
    let mut node = e;
    let mut dflt = None;
    let mut unmodelled: Vec<String> = Vec::new();

    while let ExprKind::Filter { node: inner, name, args } = &node.kind {
        let short = name.rsplit('.').next().unwrap_or(name);
        match short {
            "default" | "d" => {
                // `default(D)` and `default(D, true)`: the second argument only decides
                // whether a *falsy* value is replaced too, not what the unset value is.
                match args.args.first() {
                    Some(a) => dflt = literal_text(a),
                    None => return None,
                }
                if dflt.is_none() {
                    return None;
                }
            }
            "bool" if args.args.is_empty() => {}
            other => unmodelled.push(other.to_string()),
        }
        node = inner;
    }

    let var = var_ref(src, node)?;
    // Outermost-first reads better for the one caller that inspects it.
    unmodelled.reverse();
    Some((var, dflt, unmodelled))
}

/// A variable reference, as written.
///
/// The text comes from the node's span rather than being re-rendered, so a label quotes the
/// author's spelling instead of a normalisation of it.
fn var_ref(src: &str, e: &Expr) -> Option<VarRef> {
    let root = e.root_name()?;
    if LITERALS.contains(&root) || NOT_VARIABLES.contains(&root) {
        return None;
    }
    // A non-literal subscript — `hostvars[h].x` — is representable in the tree but not
    // resolvable: `h` is exactly the part that is not known statically. Refused, as it was
    // before the tree existed, rather than reported as a claim about `hostvars`.
    if !literal_path(e) {
        return None;
    }
    Some(VarRef { root: root.to_string(), expr: e.span.slice(src).to_string() })
}

/// Whether every accessor on the path is spellable without knowing a variable's value.
fn literal_path(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Name(_) => true,
        ExprKind::Getattr { node, .. } => literal_path(node),
        ExprKind::Getitem { node, arg } => {
            matches!(arg.kind, ExprKind::Const(JConst::Str(_) | JConst::Int(_)))
                && literal_path(node)
        }
        _ => false,
    }
}

/// A literal's value as a label would print it. `None` for anything computed.
fn literal_text(e: &Expr) -> Option<String> {
    match &e.kind {
        ExprKind::Const(JConst::Str(s)) => Some(s.clone()),
        ExprKind::Const(JConst::Int(i)) => Some(i.to_string()),
        ExprKind::Const(JConst::Float(f)) => Some(f.to_string()),
        ExprKind::Const(JConst::Bool(b)) => Some(b.to_string()),
        ExprKind::Const(JConst::None) => Some("none".to_string()),
        _ => None,
    }
}

/// The members of a literal list or tuple. Empty for anything else — `groups['servers']` is
/// a subscript, not a list, and reading it as one produced a bogus single-element membership.
fn literal_list(e: &Expr) -> Vec<String> {
    let items = match &e.kind {
        ExprKind::List(items) | ExprKind::Tuple(items) => items,
        _ => return Vec::new(),
    };
    let mut out = Vec::new();
    for item in items {
        match literal_text(item) {
            Some(v) => out.push(v),
            None => return Vec::new(),
        }
    }
    out
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

    // ------------------------------------------------ T-188: the tree, not the text

    /// T-187. Whitespace never reaches the parser, so the two spellings are not "handled the
    /// same" — they are the same question. Asserted on more than one arm, because a fix that
    /// only reached `RequiresNonEmpty` would look identical from the ticket's own example.
    /// `True`, `False` and `None` are literals, not variables — and were being reported as
    /// undefined ones, while their lowercase spellings were not.
    ///
    /// Measured against jinja2 3.1.6 rather than assumed: `env.parse("{{ X }}")` yields a
    /// `Const` for all six spellings and a `Name` for `null`, `nil` and `TRUE`. Those three
    /// are the control — a fix that simply stopped reporting anything would pass the first
    /// half of this test and fail the second.
    #[test]
    fn the_six_literal_spellings_are_not_variable_references() {
        let names = |e: &str| {
            super::variable_uses(e).into_iter().map(|(n, _, _)| n).collect::<Vec<_>>()
        };
        assert_eq!(names("True or False or None or true or false or none"), Vec::<String>::new());
        // The control: these three lex the same way but jinja2 resolves them to `Name`, so
        // they are real variable uses and must survive.
        assert_eq!(names("null or nil or TRUE"), ["null", "nil", "TRUE"]);
    }

    #[test]
    fn spacing_around_operators_cannot_change_a_verdict() {
        let pairs = [
            ("hosts | length > 0", "hosts|length>0"),
            ("skip_x | default(false) | bool", "skip_x|default(false)|bool"),
            ("mode | default('native') == 'native'", "mode|default('native')=='native'"),
            ("mode | default('a') in ['a', 'b']", "mode|default('a')in['a','b']"),
            ("x is defined", "x  is  defined"),
        ];
        for (spaced, tight) in pairs {
            assert_eq!(classify(spaced), classify(tight), "{spaced:?} vs {tight:?}");
            assert_ne!(classify(spaced), Verdict::Unknown, "{spaced:?} must classify at all");
        }
    }

    /// T-211. `d` is `default` — ansible-core registers both names for the same filter — and
    /// either may be written fully qualified. All four spellings are one shape once the
    /// filter is a node with a name rather than a substring to strip.
    #[test]
    fn every_spelling_of_the_default_filter_classifies_the_same() {
        let want = classify("skip_x | default(false)");
        assert_eq!(want, Verdict::OnlyIfSet { var: "skip_x".into() });
        for spelling in [
            "skip_x | d(false)",
            "skip_x | ansible.builtin.default(false)",
            "skip_x | ansible.builtin.d(false)",
            "skip_x|d(false)|bool",
        ] {
            let got = classify(spelling);
            assert_eq!(got, want, "{spelling:?}");
            // Both rendering surfaces, not just the verdict: a spelling that classified but
            // rendered differently would still be a difference the user sees (rule 3).
            assert_eq!(got.label(), want.label(), "{spelling:?} label");
            assert_eq!(got.requirement(), want.requirement(), "{spelling:?} requirement");
        }

        // A second arm, so the alias is not pinned to one verdict.
        let truthy = classify("skip_x | default(true)");
        assert_eq!(truthy, Verdict::UnlessCleared { var: "skip_x".into() });
        for spelling in ["skip_x | d(true)", "skip_x | ansible.builtin.d(true)"] {
            assert_eq!(classify(spelling), truthy, "{spelling:?}");
        }

        // The guard that must not have widened: `d`/`default` are aliases of one filter, and
        // recognising them says nothing about any other name in the same position. A filter
        // this module has no model for still refuses, whatever it is called.
        for unknown in [
            "skip_x | frobnicate(false)",
            "skip_x | ansible.builtin.frobnicate(false)",
            "skip_x | dd(false)",
            "skip_x | default_if_none(false)",
        ] {
            assert_eq!(classify(unknown), Verdict::Unknown, "{unknown:?}");
        }
    }

    /// A quoted operator is text, not syntax. This is the half `strip_strings` existed to
    /// fake: the old matcher cut on `|` and `==` wherever they appeared, so a value that
    /// contained one came apart.
    #[test]
    fn an_operator_inside_a_string_literal_survives_as_its_value() {
        assert_eq!(
            classify("mode | default('a|b') == 'a|b'"),
            Verdict::WhenEquals {
                var: "mode".into(),
                value: "a|b".to_string(),
                negated: false,
                matches_default: true,
            }
        );
        assert_eq!(
            classify("mode | default('x') == 'a > 0'"),
            Verdict::WhenEquals {
                var: "mode".into(),
                value: "a > 0".to_string(),
                negated: false,
                matches_default: false,
            }
        );
    }

    /// Ansible's own check: `compile_expression` refuses anything left over rather than
    /// returning a verdict about the first token. Without it these are a confident claim
    /// about `foo`, which is this ticket's failure mode reintroduced by its own fix.
    #[test]
    fn a_condition_with_trailing_junk_is_unknown_not_a_claim_about_its_first_token() {
        for cond in ["foo bar", "x }} y", "x is defined and", "hosts | length > 0 )"] {
            assert_eq!(classify(cond), Verdict::Unknown, "{cond:?}");
        }
        // The control: each prefix on its own does classify, so the refusal is about the
        // leftovers rather than about the prefix being unreadable.
        assert_ne!(classify("x is defined"), Verdict::Unknown);
        assert_ne!(classify("hosts | length > 0"), Verdict::Unknown);
    }

    /// The bar is not "parses" but "is a shape this module models". Each of these is valid
    /// Jinja that the tree represents perfectly well, and every one must still be `Unknown`
    /// — a verdict here would be an invention, which is the thing the whole module exists
    /// not to do.
    #[test]
    fn constructs_we_deliberately_do_not_model_stay_unknown() {
        let unmodelled = [
            // Arithmetic and comparison against something that is not a literal.
            "a + b > c",
            "x > y",
            // A conditional expression: two answers, and no way to say which.
            "a if b else c",
            // A filter this module has no model for. `sort` may change what `length` counts,
            // so passing it through would be a guess.
            "x | sort | length > 0",
            // A call. What it returns is a runtime question.
            "lookup('env', 'HOME')",
            "x.method()",
            // A test other than `defined`.
            "x is divisibleby 3",
            "x is sameas y",
            // Boolean structure: `and` and `or` in Jinja return an *operand*, not a bool
            // (T-114), so even the shape is not what it looks like.
            "a and b",
            "a or b",
            // Membership in something that is not a literal list.
            "x in groups['web']",
            "x in y",
            // A subscript whose key is itself a variable — `h` is exactly the part that is
            // not knowable, so the reference cannot be spelled (T-186).
            "hostvars[h].x is defined",
        ];
        for cond in unmodelled {
            assert_eq!(classify(cond), Verdict::Unknown, "{cond:?} must not be described");
        }
    }

    /// The nine conditions in `ansible/ansible` that the string matcher classified and the
    /// tree does not. Every one was a claim it had no right to make, so the drop is the fix
    /// working — but a drop in reach is exactly the kind of thing that gets waved through, so
    /// each shape is pinned here with what the old answer was.
    ///
    /// `in ('RedHat')` is the one worth reading twice. A parenthesised string is not a tuple,
    /// so this is a *substring* test. Live on ansible-core 2.21.2: `distro` of `Red` and of
    /// `Hat` both run it, and `distro in ["RedHat"]` — a real list — skips for `Red`. The old
    /// verdict, "runs when distro is one of: RedHat", was false for every substring.
    #[test]
    fn shapes_the_string_matcher_claimed_and_could_not_have_known() {
        for cond in [
            // A literal is not a variable. `parse_var_ref` took the digits as a name and
            // answered about a variable called `1`.
            "1 in [1,2,3]",
            "0 not in [1,2,3]",
            "200 is not defined",
            // A parenthesised string is a string. Membership in it is a substring test.
            "ansible_distribution in ('RedHat')",
            "ansible_distribution in ('Ubuntu')",
            // The right-hand side is a concatenation, not a literal. The old matcher took the
            // raw text after `==` as the value and reported the `+` signs as part of it.
            "result.url|default(\"\") == \"https://\" + httpbin_host + \"/get\"",
        ] {
            assert_eq!(classify(cond), Verdict::Unknown, "{cond:?} must not be described");
        }
        // The control: the shapes these were mistaken for do still classify, so the refusals
        // above are about these inputs and not about the arms having stopped working.
        assert_ne!(classify("x in ['RedHat']"), Verdict::Unknown);
        assert_ne!(classify("x is not defined"), Verdict::Unknown);
        assert_ne!(classify("x | default('') == 'https://get'"), Verdict::Unknown);
    }

    /// T-213. The four `(matches_default, negated)` framings, each pinned to what
    /// ansible-core 2.21.2 actually does with the variable unset:
    ///
    /// | condition | unset |
    /// | --- | --- |
    /// | `x \| d("a") in ["b"]` | skipping |
    /// | `x \| d("a") in ["a","b"]` | **ran** |
    /// | `x \| d("a") not in ["a","b"]` | skipping |
    /// | `x \| d("a") not in ["b"]` | **ran** |
    /// | `x in ["a"]` | **fatal** — `x` is undefined |
    /// | `x not in ["a"]` | **fatal** — `x` is undefined |
    ///
    /// The last two are why `matches_default` is an `Option`: unguarded, *neither* direction
    /// runs when unset, so `Some(false)` would be a different claim than the truth.
    #[test]
    fn a_membership_test_is_framed_by_whether_it_runs_unset() {
        let cases: &[(&str, Option<bool>, &str)] = &[
            ("x | d('a') in ['b']", Some(false), "runs only if x is one of [b]"),
            ("x | d('a') in ['a', 'b']", Some(true), "runs unless x leaves [a, b]"),
            ("x | d('a') not in ['a', 'b']", Some(false), "runs only if x is not one of [a, b]"),
            ("x | d('a') not in ['b']", Some(true), "runs unless x is one of [b]"),
            ("x in ['a']", None, "runs only if x is one of [a]"),
            ("x not in ['a']", None, "runs only if x is not one of [a]"),
        ];
        for (cond, want_default, want_label) in cases {
            let v = classify(cond);
            let Verdict::WhenIn { matches_default, .. } = &v else {
                panic!("{cond:?} did not classify as a membership test: {v:?}");
            };
            assert_eq!(matches_default, want_default, "{cond:?}");
            assert_eq!(v.label().as_deref(), Some(*want_label), "{cond:?}");
        }
        // The rule the wording carries, stated once rather than trusted to six strings:
        // "runs unless" is this module's phrase for *runs by default*, so it must appear
        // only where the measured answer above is "ran".
        for (cond, want_default, want_label) in cases {
            assert_eq!(
                want_label.starts_with("runs unless"),
                *want_default == Some(true),
                "{cond:?} is framed as running by default and is not, or the reverse"
            );
        }
    }

    /// T-213's three corpus conditions, verbatim from `debops` — the only membership tests in
    /// the eight pinned trees whose default is itself in the list, and the ones that shipped
    /// "runs only if" for a task that runs by default.
    #[test]
    fn the_corpus_conditions_that_run_by_default_say_so() {
        for cond in [
            "item.state | d('present') in ['present', 'absent']",
            "item.state | d('directory') in ['directory', 'absent']",
            "item.state | d('mounted') in ['mounted', 'present', 'unmounted']",
        ] {
            let v = classify(cond);
            assert!(
                matches!(v, Verdict::WhenIn { matches_default: Some(true), .. }),
                "{cond:?}: {v:?}"
            );
            let label = v.label().expect("classifies");
            assert!(label.starts_with("runs unless"), "{cond:?}: {label}");
        }
        // The control: drop the default's membership and the framing has to flip back, or
        // the fix is just "always say runs unless" rather than a reading of the condition.
        let v = classify("item.state | d('present') in ['absent']");
        assert!(matches!(v, Verdict::WhenIn { matches_default: Some(false), .. }), "{v:?}");
        assert_eq!(v.label().as_deref(), Some("runs only if item.state is one of [absent]"));
    }

    /// `None` is not `Some(false)`, and the difference only shows through `invert`. An
    /// unguarded membership test is a fatal error when unset, so negating it does not make it
    /// run by default — where a guard whose default misses the list does exactly that.
    #[test]
    fn inverting_a_membership_test_keeps_the_unguarded_case_unguarded() {
        let unguarded = classify("not (x in ['a'])");
        assert!(matches!(unguarded, Verdict::WhenIn { matches_default: None, .. }), "{unguarded:?}");
        assert_eq!(unguarded.label().as_deref(), Some("runs only if x is not one of [a]"));

        let guarded = classify("not (x | d('a') in ['b'])");
        assert!(
            matches!(guarded, Verdict::WhenIn { matches_default: Some(true), .. }),
            "{guarded:?}"
        );
        assert_eq!(guarded.label().as_deref(), Some("runs unless x is one of [b]"));
    }

    /// The tree can represent far more than the old matcher could, and that is exactly why
    /// the reach has to be pinned: growing it is a decision, not a side effect. These are the
    /// shapes that *do* classify, one per arm.
    #[test]
    fn every_arm_still_has_a_shape_that_reaches_it() {
        use Verdict::*;
        let cases: &[(&str, Verdict)] = &[
            ("false", Never),
            ("true", Always),
            ("x | default(false)", OnlyIfSet { var: "x".into() }),
            ("x | default(true)", UnlessCleared { var: "x".into() }),
            ("not (x | default(false))", UnlessSet { var: "x".into() }),
            ("x is defined", RequiresDefined { var: "x".into(), negated: false }),
            ("x is not defined", RequiresDefined { var: "x".into(), negated: true }),
            ("x | length > 0", RequiresNonEmpty { var: "x".into() }),
            (
                "m | default('a') == 'a'",
                WhenEquals {
                    var: "m".into(),
                    value: "a".into(),
                    negated: false,
                    matches_default: true,
                },
            ),
            (
                // The default is itself in the list, so this runs when `m` is unset (T-213).
                "m | default('a') in ['a', 'b']",
                WhenIn {
                    var: "m".into(),
                    values: vec!["a".into(), "b".into()],
                    negated: false,
                    matches_default: Some(true),
                },
            ),
        ];
        for (cond, want) in cases {
            assert_eq!(&classify(cond), want, "{cond:?}");
        }
    }

    /// T-104: the name a `hostvars[...]` read actually consults, in both spellings. The
    /// root extractor sees none of it — `hostvars` is injected and dropped, `app_port` is
    /// an attribute and skipped — so without this the name produces no use at all.
    #[test]
    fn hostvars_reads_are_extracted_in_both_spellings() {
        let names = |e: &str| -> Vec<String> {
            hostvars_uses(e).into_iter().map(|(n, _, _)| n).collect()
        };
        assert_eq!(names("hostvars['web01'].app_port"), ["app_port"]);
        assert_eq!(names("hostvars[\"web01\"]['app_port']"), ["app_port"]);
        assert_eq!(names("hostvars[inventory_hostname].app_port"), ["app_port"]);
        // Deeper access still names the variable, not the sub-key.
        assert_eq!(names("hostvars['w'].app_port.children[0]"), ["app_port"]);
        // Several in one expression, each kept separately.
        assert_eq!(
            names("hostvars['a'].one ~ hostvars['b'].two"),
            ["one", "two"]
        );
        // The span covers the name alone, so hover highlights it and nothing else.
        let e = "hostvars['web01']['app_port']";
        let (_, s, t) = hostvars_uses(e).remove(0);
        assert_eq!(&e[s..t], "app_port");
    }

    /// [`hostvars_host_uses`] — the *host* half, which feeds an ERROR rather than a link, so
    /// each filter it adds over [`hostvars_host_keys`] gets asserted here.
    #[test]
    fn hostvars_host_uses_keeps_only_what_an_error_may_stand_on() {
        let hosts = |src: &str| -> Vec<String> {
            let nodes = crate::parse::Document::new(src.to_string()).parse().unwrap();
            hostvars_host_uses(src, &nodes).into_iter().map(|(n, _, _)| n).collect()
        };
        // A folded block scalar, so the expression reaches the document text unescaped —
        // a double-quoted YAML scalar would put a backslash before every inner quote, and
        // this scan reads the raw text, not the parsed value.
        let msg = |e: &str| format!("- hosts: all\n  tasks:\n    - debug:\n        msg: >-\n          {e}\n");

        // Both quotings, and whitespace inside the subscript.
        assert_eq!(hosts(&msg("{{ hostvars['web01'].x }}")), ["web01"]);
        assert_eq!(hosts(&msg("{{ hostvars[\"web01\"].x }}")), ["web01"]);
        assert_eq!(hosts(&msg("{{ hostvars[ 'web01' ].x }}")), ["web01"]);

        // Several on one line, each its own use.
        assert_eq!(hosts(&msg("{{ hostvars['a'].x }}{{ hostvars['b'].y }}")), ["a", "b"]);

        // `hostvars` as part of a longer identifier is not `hostvars`.
        assert!(hosts(&msg("{{ myhostvars['a'].x }}")).is_empty());
        assert!(hosts(&msg("{{ result.hostvars['a'].x }}")).is_empty());

        // In a comment it is prose. 2 of the reference corpus's 23 literal uses are this.
        let commented = "# {{ hostvars['web01'].x }}\n- hosts: all\n  tasks: []\n";
        assert!(hosts(commented).is_empty());

        // A group name reached through the inner lookup is not the host key.
        assert!(hosts(&msg("{{ hostvars[groups['web'][0]].x }}")).is_empty());

        // Swallowed by the expression it sits in, so nothing fails and nothing is claimed.
        assert!(hosts(&msg("{{ hostvars['w'].x | default('z') }}")).is_empty());
        assert!(hosts(&msg("{{ hostvars['w'].x if hostvars['w'] is defined else '' }}")).is_empty());
        // ...but a default in a *neighbouring* expression rescues nothing.
        assert_eq!(hosts(&msg("{{ hostvars['w'].x }} {{ y | default(1) }}")), ["w"]);

        // A host key is read from a `when:` exactly as from a value — same expression
        // language, and a rule that only looked at `msg:` would miss half the corpus.
        let when = "- hosts: all\n  tasks:\n    - debug:\n        msg: hi\n      \
                    when: hostvars['w'].ready\n";
        assert_eq!(hosts(when), ["w"]);
    }

    /// The shapes it must refuse. Each would be a claim about a name Ansible never reads
    /// there — the expensive kind of wrong, since it ends in a hover pointing somewhere.
    #[test]
    fn hostvars_extraction_refuses_what_it_cannot_know() {
        let names = |e: &str| -> Vec<String> {
            hostvars_uses(e).into_iter().map(|(n, _, _)| n).collect()
        };
        // The read name is itself a variable — unknowable without evaluating it.
        assert!(names("hostvars[h][wanted]").is_empty());
        // No read at all: the whole host dict, handed to a filter.
        assert!(names("hostvars['web01'] | to_json").is_empty());
        assert!(names("hostvars").is_empty());
        // Somebody else's attribute that happens to be spelled the same.
        assert!(names("result.hostvars['a'].x").is_empty());
        // Inside a string literal it is text, not an expression.
        assert!(names("'hostvars[\\'a\\'].x'").is_empty());
        // A `]` inside the host name must not close the subscript early.
        assert_eq!(names("hostvars['we]b01'].app_port"), ["app_port"]);
        // Not an identifier, so not a name we can look up.
        assert!(names("hostvars['w']['a-b']").is_empty());
    }

    /// T-171: the host half. Claimed only when the cursor is inside the host name itself,
    /// so the two names on one line stay separately clickable.
    #[test]
    fn a_literal_hostvars_key_names_its_host_under_the_cursor() {
        let t = "msg: {{ hostvars['web01'].web01_ib_ip }}";
        let at = t.find("web01'").unwrap();
        assert_eq!(hostvars_host_key_at(t, at).unwrap().0, "web01");
        // The span is the name inside the quotes.
        let (_, s, e) = hostvars_host_key_at(t, at).unwrap();
        assert_eq!(&t[s..e], "web01");
        // Outside the key — on the variable, on `hostvars`, before the read — it declines,
        // so it can never steal a click the variable half should answer.
        assert!(hostvars_host_key_at(t, t.find("web01_ib_ip").unwrap()).is_none());
        assert!(hostvars_host_key_at(t, t.find("hostvars").unwrap()).is_none());
        assert!(hostvars_host_key_at(t, 0).is_none());
        // A non-literal key names no host that can be known here.
        let d = "{{ hostvars[inventory_hostname].x }}";
        assert!(hostvars_host_key_at(d, d.find("inventory_hostname").unwrap()).is_none());
        // Somebody else's attribute spelled the same is not the magic dict.
        let o = "{{ result.hostvars['web01'].x }}";
        assert!(hostvars_host_key_at(o, o.find("web01").unwrap()).is_none());
    }

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
                matches_default: None,
            }
        );
        assert!(v.label().unwrap().starts_with("runs only if ap_operation is one of"));
        // Unguarded, so an unset `mode` is a fatal error and not a skip — measured on
        // 2.21.2. "Runs unless" would claim it runs by default, which it does not (T-213).
        let neg = classify("mode not in ['a', 'b']");
        assert_eq!(neg.label().as_deref(), Some("runs only if mode is not one of [a, b]"));
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

    /// T-186. Every arm that carries a variable is fed by the same two extractors, and those
    /// kept only the root of a dotted path — so the hint made a claim about `r` when the
    /// condition was about `r.stdout`. For a registered result that is the dangerous
    /// direction: `r` is a dict with `changed` and `rc` in it and is never empty, so the hint
    /// promised a run that will be skipped. Asserted per arm rather than on the one that
    /// surfaced it, because the defect is in the shared extractor.
    #[test]
    fn a_dotted_path_is_named_in_full_never_reduced_to_its_root() {
        let cases = [
            ("not (r.skip | default(false) | bool)", "runs unless r.skip is set", "r.skip unset"),
            (
                "r.enabled | default(true) | bool",
                "runs unless r.enabled is false",
                "r.enabled not false",
            ),
            ("r.flag | default(false) | bool", "runs only if r.flag is set", "r.flag set"),
            (
                "r.mode | default('native') == 'native'",
                "runs unless r.mode changes from native",
                "r.mode = native",
            ),
            ("r.mode in ['a', 'b']", "runs only if r.mode is one of [a, b]", "r.mode in [a, b]"),
            ("r.stdout is defined", "runs only if r.stdout is set", "r.stdout set"),
            ("r.stdout | length > 0", "runs only if r.stdout is non-empty", "r.stdout non-empty"),
        ];
        for (cond, label, requirement) in cases {
            let v = classify(cond);
            assert_eq!(v.label().as_deref(), Some(label), "{cond}");
            assert_eq!(v.requirement().as_deref(), Some(requirement), "{cond}");
            // The root stays separately available: it is what a definition lookup or
            // provenance walk resolves, and widening `var` to hold the whole path would fix
            // the label by handing every other consumer a name it cannot look up.
            assert_eq!(v.var(), Some("r"), "{cond}");
        }
        // The control. A bare name has no accessor, so root and label are the same word and
        // this row must come out exactly as it did before the split existed.
        let plain = classify("hosts | length > 0");
        assert_eq!(plain, Verdict::RequiresNonEmpty { var: "hosts".into() });
        assert_eq!(plain.label().unwrap(), "runs only if hosts is non-empty");
        assert_eq!(plain.var(), Some("hosts"));
    }

    /// The rest of the accessor shapes, all real Jinja. The floor T-186 settled on is: name
    /// the path when every step of it is literal, and refuse the whole reference otherwise
    /// rather than reporting a root the condition never talked about.
    #[test]
    fn accessor_paths_are_named_whole_or_refused() {
        for (cond, want) in [
            ("r['stdout'] | length > 0", "runs only if r['stdout'] is non-empty"),
            ("r.results[0].stdout | length > 0", "runs only if r.results[0].stdout is non-empty"),
        ] {
            let v = classify(cond);
            assert_eq!(v.label().as_deref(), Some(want), "{cond}");
            assert_eq!(v.var(), Some("r"), "{cond}");
        }
        // A subscript that is itself a variable: the key is unknown here, so the path cannot
        // be stated. `hostvars` is the root — never `h` — but naming a path we cannot spell
        // is the T-186 mistake again, so this declines instead.
        assert_eq!(classify("hostvars[h].stdout | length > 0"), Verdict::Unknown);
        assert_eq!(classify("hostvars[h].mode | default('a') == 'a'"), Verdict::Unknown);
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
        let refs = crate::references::extract(&nodes).refs;
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
        // T-186, pinned by its exact wording: the label is the demo's claim, and the claim
        // is that the hint names the accessor path rather than the variable it hangs off.
        assert!(
            has(&|v| v.label().as_deref() == Some("runs only if demo_result.stdout is non-empty")),
            "the accessor hint stopped naming the path (T-186)"
        );
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

    /// T-032's classification rate, re-measurable. Env-gated in the T-184 shape because the
    /// trees it needs are never committed.
    ///
    /// `ANSIBLE_CORPUS` may be one tree or a directory of them. Every immediate subdirectory
    /// is reported on its own line and the blended total is printed last, because the rate is
    /// **per-repo house style** rather than a global property — T-211 measured one tree using
    /// `d(` 852 times and another using it once, and a single averaged number hides exactly
    /// that.
    #[test]
    #[ignore = "corpus gate: ANSIBLE_CORPUS=<path> cargo test -p ansible-core when_coverage -- --ignored --nocapture"]
    fn when_coverage() {
        fn pct(n: usize, d: usize) -> usize {
            if d == 0 { 0 } else { n * 100 / d }
        }

        let Ok(root) = std::env::var("ANSIBLE_CORPUS") else { return };
        let root = std::path::PathBuf::from(root);
        assert!(root.is_dir(), "ANSIBLE_CORPUS={} is not a directory", root.display());

        let mut trees: Vec<std::path::PathBuf> = std::fs::read_dir(&root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        trees.sort();
        if trees.is_empty() {
            trees.push(root.clone());
        }

        let (mut t_files, mut t_sites, mut t_hit, mut t_cl, mut t_cl_hit) = (0, 0, 0, 0, 0);
        for tree in &trees {
            let (mut files, mut sites, mut hit, mut cl, mut cl_hit) = (0, 0, 0, 0, 0);
            for f in crate::workspace::yaml_files(tree) {
                let Ok(text) = std::fs::read_to_string(&f) else { continue };
                let Some(nodes) = crate::parse_libyaml::parse_lenient(&text) else { continue };
                files += 1;
                for site in crate::expressions::sites(&nodes) {
                    sites += 1;
                    if classify_all(&site.clauses) != Verdict::Unknown {
                        hit += 1;
                    }
                    for c in &site.clauses {
                        cl += 1;
                        if classify(c) != Verdict::Unknown {
                            cl_hit += 1;
                        }
                    }
                }
            }
            let name = tree.file_name().map_or_else(String::new, |n| n.to_string_lossy().into());
            println!(
                "{name:22} files={files:5} sites={sites:5} classified={hit:5} ({:2}%)  \
                 clauses={cl:5} classified={cl_hit:5} ({:2}%)",
                pct(hit, sites),
                pct(cl_hit, cl),
            );
            t_files += files;
            t_sites += sites;
            t_hit += hit;
            t_cl += cl;
            t_cl_hit += cl_hit;
        }
        println!(
            "{:22} files={t_files:5} sites={t_sites:5} classified={t_hit:5} ({:2}%)  \
             clauses={t_cl:5} classified={t_cl_hit:5} ({:2}%)",
            "TOTAL",
            pct(t_hit, t_sites),
            pct(t_cl_hit, t_cl),
        );
        // A sweep that found nothing measures nothing; it must not read as a clean result.
        assert!(t_sites > 0, "no condition sites found under {}", root.display());
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
        // 16/111 and 10/111 as measured. Low by design, and in line with the full sweep
        // (166/1313 across the collections, 221/2261 across kubespray) — the sample tracks
        // the corpus it came from rather than being cherry-picked for a flattering number.
        // It was 14 before T-186: reading the accessor path instead of cutting it off also
        // reads two `acme_*[N].subject_key_identifier is defined` rows that used to be
        // refused outright, so the correctness fix bought reach rather than costing it.
        assert!(classified >= 16, "only {classified}/{} classified", REAL_WHENS.len());
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

    /// The rule itself is right; only its input is missing. With the loop flag set — what a
    /// loop-aware caller would pass — these three come out clean, which is what proves the
    /// defect is the flag and not the rule. Live, and the half of T-139 that already holds.
    #[test]
    fn item_from_an_including_loop_is_well_formed_once_the_loop_is_known() {
        for c in ITEM_FROM_AN_INCLUDING_LOOP {
            assert!(problems(c, true).is_empty(), "{c} is otherwise well-formed");
        }
    }

    /// T-139: `item` **is** defined in these three — the loop sits on the `include_tasks`
    /// that pulls their file in, which [`problems`] never sees, so we report a false
    /// positive.
    ///
    /// This used to assert the false positive was present, with a comment saying to invert it
    /// on the fix. That is a green test claiming the wrong answer is correct (rule 7), so it
    /// now states the answer we owe and is ignored until we give it. The control above keeps
    /// the half that passes today.
    #[test]
    #[ignore = "asserts the loop-aware answer we do not give yet — T-139"]
    fn item_from_an_including_loop_is_not_flagged() {
        for c in ITEM_FROM_AN_INCLUDING_LOOP {
            assert!(
                !problems(c, false).contains(&Problem::ItemWithoutLoop),
                "{c}: the including loop defines `item`"
            );
        }
    }
}
