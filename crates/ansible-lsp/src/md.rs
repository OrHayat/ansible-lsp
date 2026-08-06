//! Markdown for hovers, in one place (T-082).
//!
//! Every tooltip used to be a `String` grown with `format!` and `push_str`, with the syntax
//! written inline as literals in eight different functions. Structure existed only as
//! punctuation, so composing two hovers was string surgery — and nothing escaped the values,
//! which are file paths, variable names and raw `when:` expressions out of user files.
//!
//! The type is the point. [`Md::line`] takes an [`Inline`], and the only ways to make one
//! are [`raw`] (authored prose, `&'static str`), [`text`] (escaped), [`code`] (fenced) and
//! [`link`] (label escaped). A `String` read from a user's file cannot become an `Inline`
//! without passing through an escaping constructor, so the bug this module exists to fix
//! cannot be reintroduced by forgetting — it stops compiling instead.
//!
//! Deliberately not a markdown crate: the output surface is five constructs wide, and a
//! dependency would be larger than the code it replaces.

use std::ops::Add;

use tower_lsp::lsp_types::Url;

/// A run of markdown that is already safe to emit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inline(String);

impl Inline {
    pub fn bold(self) -> Inline {
        Inline(format!("**{}**", self.0))
    }

    pub fn italic(self) -> Inline {
        Inline(format!("_{}_", self.0))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Add<Inline> for Inline {
    type Output = Inline;
    fn add(mut self, rhs: Inline) -> Inline {
        self.0.push_str(&rhs.0);
        self
    }
}

/// Appending authored prose. `&'static str` for the same reason [`raw`] takes one.
impl Add<&'static str> for Inline {
    type Output = Inline;
    fn add(mut self, rhs: &'static str) -> Inline {
        self.0.push_str(rhs);
        self
    }
}

/// Authored prose, emitted as written.
///
/// `&'static str` is the guard, not a lifetime convenience: a value read out of a user's
/// file is a `String` and never `'static`, so it cannot reach here. Anything dynamic has to
/// choose [`text`] or [`code`], which is precisely the decision that was being skipped.
pub fn raw(s: &'static str) -> Inline {
    Inline(s.to_string())
}

/// `"Tried:".bold()` rather than `raw("Tried:").bold()`, which is most of the call sites.
///
/// Implemented for `&'static str` only — deliberately not for `&str` or `String`. That is
/// the whole safety property: a literal can style itself, a value out of a user's file
/// cannot, and has to pass through [`text`] or [`code`] first.
pub trait Prose {
    fn md(self) -> Inline;
    fn bold(self) -> Inline;
    fn italic(self) -> Inline;
}

impl Prose for &'static str {
    fn md(self) -> Inline {
        raw(self)
    }
    fn bold(self) -> Inline {
        raw(self).bold()
    }
    fn italic(self) -> Inline {
        raw(self).italic()
    }
}

pub fn empty() -> Inline {
    Inline(String::new())
}

/// An inline code span that survives its content.
///
/// A `when:` expression can contain backticks, and the naive `` format!("`{s}`") `` ends the
/// span early, rendering the rest as prose. CommonMark fences a span with any run of
/// backticks not appearing inside it, and strips one leading *and* trailing space — which is
/// what makes a value that itself starts or ends with a backtick expressible at all.
pub fn code(s: &str) -> Inline {
    let longest = s.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest + 1);
    // The stripping rule only fires when both sides have a space, so pad both or neither.
    let pad = if s.starts_with('`') || s.ends_with('`') { " " } else { "" };
    Inline(format!("{fence}{pad}{s}{pad}{fence}"))
}

/// Text from a user file, with the characters a renderer would act on neutralised.
///
/// Only the ones that can fire mid-line: emphasis, code, links, raw HTML. Not `.`, `-` or
/// `#`, which are block-level and only matter at the start of a line — escaping those would
/// put backslashes through most of the prose here and prevent nothing.
///
/// `_` is escaped only at a word boundary. CommonMark disallows intraword `_` emphasis, so
/// `use_ssl` cannot emphasize; Ansible variable names are mostly underscores, and escaping
/// every one would mark up nearly every condition label to prevent nothing.
pub fn text(s: &str) -> Inline {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for (i, &c) in chars.iter().enumerate() {
        let escape = match c {
            '\\' | '`' | '*' | '[' | ']' | '<' | '>' => true,
            '_' => {
                let alnum = |j: Option<usize>| {
                    j.and_then(|j| chars.get(j)).is_some_and(|c| c.is_alphanumeric())
                };
                !(alnum(i.checked_sub(1)) && alnum(Some(i + 1)))
            }
            _ => false,
        };
        if escape {
            out.push('\\');
        }
        out.push(c);
    }
    Inline(out)
}

/// `[label](uri)`, optionally anchored at a line. The label is user-derived — a path can
/// contain `[` or `]` — so it is escaped; `Url` has already percent-encoded the target.
pub fn link(label: &str, url: &Url, line: Option<usize>) -> Inline {
    let label = text(label).0;
    Inline(match line {
        Some(n) => format!("[{label}]({url}#L{n})"),
        None => format!("[{label}]({url})"),
    })
}

/// A hover document: blocks separated by a blank line, bullets by a single newline.
#[derive(Default, Clone, Debug)]
pub struct Md {
    out: String,
    /// Set by [`Md::gap`]: the next bullet opens its own block instead of hanging off the
    /// line above. Both shapes are real — `Tried:` wants its list attached to the lead
    /// line, the variable hover wants a blank line first.
    gap: bool,
}

impl Md {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a block. Blocks are separated by a blank line — the joining rule T-078 had to
    /// invent at its call site (`format!("{b}\n\n{g}")`) lives here now.
    pub fn line(mut self, s: Inline) -> Self {
        if !self.out.is_empty() {
            self.out.push_str("\n\n");
        }
        self.gap = false;
        self.out.push_str(&s.0);
        self
    }

    /// The next bullet begins its own block rather than hanging off the line above.
    pub fn gap(mut self) -> Self {
        self.gap = true;
        self
    }

    fn bullet(mut self, prefix: &str, s: Inline) -> Self {
        if !self.out.is_empty() {
            self.out.push_str(if self.gap { "\n\n" } else { "\n" });
        }
        self.gap = false;
        self.out.push_str(prefix);
        self.out.push_str(&s.0);
        self
    }

    /// A `- ` bullet.
    pub fn item(self, s: Inline) -> Self {
        self.bullet("- ", s)
    }

    /// A nested bullet — one level in, for a provenance chain hanging off its definition.
    pub fn subitem(self, s: Inline) -> Self {
        self.bullet("  - ", s)
    }

    pub fn items(self, items: impl IntoIterator<Item = Inline>) -> Self {
        items.into_iter().fold(self, Md::item)
    }

    /// Append another document's blocks, keeping the blank line between them. An empty
    /// document on either side contributes nothing — no stray separator.
    pub fn concat(mut self, other: Md) -> Self {
        if other.out.is_empty() {
            return self;
        }
        if self.out.is_empty() {
            return other;
        }
        self.out.push_str("\n\n");
        self.out.push_str(&other.out);
        self
    }

    /// `self` then `other`, when there is one — the shape of every hover that appends an
    /// optional guard or provenance block.
    pub fn maybe(self, other: Option<Md>) -> Self {
        match other {
            Some(o) => self.concat(o),
            None => self,
        }
    }

    pub fn render(self) -> String {
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug this module exists for: `when:` expressions are echoed verbatim whenever
    /// `condition::classify` has no label for them, which is the common case.
    #[test]
    fn a_backtick_in_a_value_does_not_end_its_code_span() {
        assert_eq!(code("foo").as_str(), "`foo`");
        assert_eq!(code("a `b` c").as_str(), "``a `b` c``");
        assert_eq!(code("a ``b`` c").as_str(), "```a ``b`` c```");
        // Leading/trailing backticks need the padding, or the fence reads as longer.
        assert_eq!(code("`x").as_str(), "`` `x ``");
        assert_eq!(code("x`").as_str(), "`` x` ``");
    }

    #[test]
    fn emphasis_characters_in_user_text_are_neutralised() {
        assert_eq!(text("a*b").as_str(), "a\\*b");
        assert_eq!(text("_x_").as_str(), "\\_x\\_");
        assert_eq!(text("a[b](c)").as_str(), "a\\[b\\](c)");
        assert_eq!(text("<tag>").as_str(), "\\<tag\\>");
        // Block-level punctuation can't fire mid-line, so it is left alone.
        assert_eq!(text("roles/a-b.yml").as_str(), "roles/a-b.yml");
    }

    /// Ansible variable names are mostly underscores. Escaping every one would mark up
    /// nearly every condition label to prevent an emphasis CommonMark disallows anyway.
    #[test]
    fn an_intraword_underscore_is_left_alone() {
        assert_eq!(text("use_ssl").as_str(), "use_ssl");
        assert_eq!(text("runs unless enable_tls is set").as_str(), "runs unless enable_tls is set");
        assert_eq!(text("_lead").as_str(), "\\_lead");
        assert_eq!(text("trail_").as_str(), "trail\\_");
        assert_eq!(text("a __b__ c").as_str(), "a \\_\\_b\\_\\_ c");
    }

    #[test]
    fn inlines_compose_with_authored_prose() {
        assert_eq!((raw("→ ") + code("a.yml")).bold().as_str(), "**→ `a.yml`**");
        assert_eq!(
            (raw("Runs only when ") + raw("all").bold() + " hold:").as_str(),
            "Runs only when **all** hold:"
        );
    }

    #[test]
    fn blocks_are_separated_by_a_blank_line_and_bullets_by_one_newline() {
        let md = Md::new()
            .line(raw("Tried:").bold())
            .item(code("a.yml"))
            .item(code("b.yml"))
            .line(raw("tail"));
        assert_eq!(md.render(), "**Tried:**\n- `a.yml`\n- `b.yml`\n\ntail");
    }

    /// T-078 needed a module's provenance *and* its guard in one tooltip and joined them
    /// with a hand-written `\n\n` at the call site. This is that rule, once.
    #[test]
    fn concat_joins_blocks_and_ignores_empty_sides() {
        let a = Md::new().line(raw("a"));
        let b = Md::new().line(raw("b"));
        assert_eq!(a.clone().concat(b).render(), "a\n\nb");
        assert_eq!(a.clone().concat(Md::new()).render(), "a");
        assert_eq!(Md::new().concat(a.clone()).render(), "a");
        assert_eq!(a.maybe(None).render(), "a");
    }

    #[test]
    fn a_link_escapes_its_label_but_not_its_uri() {
        let u = Url::parse("file:///tmp/a%20b.yml").unwrap();
        assert_eq!(
            link("a[1].yml", &u, Some(7)).as_str(),
            "[a\\[1\\].yml](file:///tmp/a%20b.yml#L7)"
        );
        assert_eq!(link("x.yml", &u, None).as_str(), "[x.yml](file:///tmp/a%20b.yml)");
    }
}
