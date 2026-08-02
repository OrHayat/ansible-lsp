//! Ansible's free-form value splitter: `parsing/splitter.py` + `parsing/quoting.py`
//! (ansible-core 2.21.2), behaviour-faithful. Every module whose value is a plain string
//! goes through `parse_kv` before the module sees it — `include_vars: x.yml name=db` and
//! 1.x-style `copy: src=a dest=b` are this machinery, not module features.
//!
//! `check_raw` mirrors `FREEFORM_ACTIONS` (the shell/command family): there, a `k=v` token
//! stays part of the raw command unless its key is one of the shell-control options.
//! Whether the raw remainder *survives* at all is the caller's concern (`RAW_PARAM_MODULES`).

use regex::Regex;
use std::sync::LazyLock;

/// `parse_kv` output. Ansible returns one dict with `_raw_params` smuggled in as a key;
/// splitting the two is the only intentional API difference.
#[derive(Debug, Default, PartialEq)]
pub struct ParsedKv {
    /// `k=v` tokens in order, later duplicate keys overwriting earlier (dict semantics).
    pub options: Vec<(String, String)>,
    /// Non-`k=v` tokens rejoined with their original spacing, if any.
    pub raw_params: Option<String>,
}

impl ParsedKv {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.options.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
}

/// The shell-control options that stay `k=v` even under `check_raw`.
const RAW_OK_KEYS: [&str; 8] = [
    "creates", "removes", "chdir", "executable", "warn", "stdin", "stdin_add_newline",
    "strip_empty_ends",
];

pub fn parse_kv(args: &str, check_raw: bool) -> Result<ParsedKv, String> {
    let mut out = ParsedKv::default();
    let mut raw = Vec::new();
    for orig in split_args(args)? {
        let x = decode_escapes(&orig);
        // The first `=` past position 0 whose predecessor isn't a backslash.
        let chars: Vec<(usize, char)> = x.char_indices().collect();
        let pos = chars
            .iter()
            .enumerate()
            .skip(1)
            .find(|(i, (_, c))| *c == '=' && chars[i - 1].1 != '\\')
            .map(|(_, (b, _))| *b);
        match pos {
            Some(p) => {
                let (k, v) = (&x[..p], &x[p + 1..]);
                if check_raw && !RAW_OK_KEYS.contains(&k) {
                    raw.push(orig);
                } else {
                    let (k, v) = (k.trim().to_string(), unquote(v.trim()).to_string());
                    match out.options.iter_mut().find(|(ok, _)| *ok == k) {
                        Some(entry) => entry.1 = v,
                        None => out.options.push((k, v)),
                    }
                }
            }
            // `=` present but every one is escaped or leading: raw, with `\=` unescaped —
            // the one branch where Python keeps the *decoded* token.
            None if x.contains('=') => raw.push(x.replace("\\=", "=")),
            None => raw.push(orig),
        }
    }
    if !raw.is_empty() {
        out.raw_params = Some(join_args(&raw));
    }
    Ok(out)
}

/// Strip one layer of matching outer quotes, unless the closing quote is escaped.
pub fn unquote(s: &str) -> &str {
    let c: Vec<char> = s.chars().collect();
    let n = c.len();
    if n > 1 && c[0] == c[n - 1] && matches!(c[0], '"' | '\'') && c[n - 2] != '\\' {
        &s[c[0].len_utf8()..s.len() - c[n - 1].len_utf8()]
    } else {
        s
    }
}

/// Whitespace split that keeps quoted strings and jinja2 blocks (`{{ }}`, `{% %}`,
/// `{# #}`) intact, preserving inner spacing. Errors on unbalanced quotes/blocks.
pub fn split_args(args: &str) -> Result<Vec<String>, String> {
    if args.is_empty() {
        return Ok(Vec::new());
    }
    let mut params: Vec<String> = Vec::new();
    let items: Vec<&str> = args.split('\n').collect();

    let mut quote_char: Option<char> = None;
    let mut inside_quotes = false;
    let mut print_depth = 0i64; // {{ }}
    let mut block_depth = 0i64; // {% %}
    let mut comment_depth = 0i64; // {# #}

    for (itemidx, item) in items.iter().enumerate() {
        let tokens: Vec<&str> = item.split(' ').collect();
        let mut line_continuation = false;
        for (idx, token) in tokens.iter().enumerate() {
            // Consecutive spaces: keep them, so raw params rejoin verbatim.
            if token.is_empty() && idx != 0 {
                if params.is_empty() {
                    params.push(String::new());
                }
                params.last_mut().unwrap().push(' ');
                continue;
            }
            if *token == "\\" && !inside_quotes {
                line_continuation = true;
                continue;
            }
            let was_inside_quotes = inside_quotes;
            quote_char = quote_state(token, quote_char);
            inside_quotes = quote_char.is_some();

            let mut appended = false;
            let in_block = print_depth > 0 || block_depth > 0 || comment_depth > 0;
            if inside_quotes && !was_inside_quotes && !in_block {
                params.push(token.to_string());
                appended = true;
            } else if in_block || inside_quotes || was_inside_quotes {
                if params.is_empty() {
                    params.push(String::new());
                }
                let last = params.last_mut().unwrap();
                if !(idx == 0 && was_inside_quotes) && idx > 0 {
                    last.push(' ');
                }
                last.push_str(token);
                appended = true;
            }

            for (depth, open, close) in [
                (&mut print_depth, "{{", "}}"),
                (&mut block_depth, "{%", "%}"),
                (&mut comment_depth, "{#", "#}"),
            ] {
                let prev = *depth;
                *depth = (*depth + token.matches(open).count() as i64
                    - token.matches(close).count() as i64)
                    .max(0);
                if *depth != prev && !appended {
                    params.push(token.to_string());
                    appended = true;
                }
            }

            if print_depth == 0
                && block_depth == 0
                && comment_depth == 0
                && !inside_quotes
                && !appended
                && !token.is_empty()
            {
                params.push(token.to_string());
            }
        }
        if items.len() > 1 && itemidx != items.len() - 1 && !line_continuation {
            if params.is_empty() {
                params.push(String::new());
            }
            params.last_mut().unwrap().push('\n');
        }
    }

    if print_depth > 0 || block_depth > 0 || comment_depth > 0 || inside_quotes {
        return Err(format!(
            "failed at splitting arguments, either an unbalanced jinja2 block or quotes: {args}"
        ));
    }
    Ok(params)
}

/// Rejoin split params with the whitespace `split_args` preserved.
pub fn join_args(parts: &[String]) -> String {
    let mut result = String::new();
    for p in parts {
        if !result.is_empty() && !result.ends_with('\n') {
            result.push(' ');
        }
        result.push_str(p);
    }
    result
}

/// Quote state after `token`: `Some(c)` while inside an unterminated `c`-quoted string.
fn quote_state(token: &str, mut quote_char: Option<char>) -> Option<char> {
    let chars: Vec<char> = token.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        let escaped = i > 0 && chars[i - 1] == '\\';
        if matches!(c, '"' | '\'') && !escaped {
            match quote_char {
                Some(q) if q == *c => quote_char = None,
                Some(_) => {}
                None => quote_char = Some(*c),
            }
        }
    }
    quote_char
}

static ESCAPES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"\\U[0-9a-fA-F]{8}|\\u[0-9a-fA-F]{4}|\\x[0-9a-fA-F]{2}|\\N\{[^}]+\}|\\[\\'"abfnrtv]"#,
    )
    .unwrap()
});

/// Python `unicode-escape` over the sequences Ansible's regex matches. `\N{...}` (Unicode
/// names) is passed through untouched — supporting it means shipping the name table, and
/// no real free-form value uses it.
fn decode_escapes(s: &str) -> String {
    ESCAPES
        .replace_all(s, |caps: &regex::Captures| {
            let m = &caps[0];
            match &m[1..2] {
                "U" | "u" | "x" => u32::from_str_radix(&m[2..], 16)
                    .ok()
                    .and_then(char::from_u32)
                    .map(String::from)
                    .unwrap_or_else(|| m.to_string()),
                "N" => m.to_string(),
                _ => match m.chars().nth(1).unwrap() {
                    '\\' => "\\".into(),
                    '\'' => "'".into(),
                    '"' => "\"".into(),
                    'a' => "\x07".into(),
                    'b' => "\x08".into(),
                    'f' => "\x0C".into(),
                    'n' => "\n".into(),
                    'r' => "\r".into(),
                    't' => "\t".into(),
                    'v' => "\x0B".into(),
                    _ => m.to_string(),
                },
            }
        })
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kv(s: &str) -> ParsedKv {
        parse_kv(s, false).expect("balanced")
    }

    #[test]
    fn kv_options_and_quoted_values() {
        let p = kv(r#"a=b c="foo bar""#);
        assert_eq!(p.get("a"), Some("b"));
        assert_eq!(p.get("c"), Some("foo bar"));
        assert_eq!(p.raw_params, None);
    }

    #[test]
    fn bare_tokens_join_into_raw_params() {
        let p = kv("x.yml name=db");
        assert_eq!(p.raw_params.as_deref(), Some("x.yml"));
        assert_eq!(p.get("name"), Some("db"));
    }

    #[test]
    fn jinja_blocks_are_not_split() {
        let p = kv("dest={{ base + '/x' }} mode=0644");
        assert_eq!(p.get("dest"), Some("{{ base + '/x' }}"));
        assert_eq!(p.get("mode"), Some("0644"));
    }

    #[test]
    fn consecutive_spaces_survive_in_raw_params() {
        let p = kv("echo  hi");
        assert_eq!(p.raw_params.as_deref(), Some("echo  hi"));
    }

    #[test]
    fn check_raw_keeps_command_equals_raw_but_takes_shell_options() {
        let p = parse_kv("echo a=b creates=/tmp/x", true).unwrap();
        assert_eq!(p.raw_params.as_deref(), Some("echo a=b"));
        assert_eq!(p.get("creates"), Some("/tmp/x"));
    }

    #[test]
    fn escaped_equals_token_is_raw_and_unescaped() {
        let p = kv(r"a\=b");
        assert_eq!(p.raw_params.as_deref(), Some("a=b"));
        assert!(p.options.is_empty());
    }

    #[test]
    fn leading_equals_is_raw() {
        assert_eq!(kv("=foo").raw_params.as_deref(), Some("=foo"));
    }

    #[test]
    fn value_containing_equals_splits_at_the_first() {
        // The T-046 case: a path with `=` becomes an option named after its prefix.
        let p = kv("vars/we=ird.yml");
        assert_eq!(p.get("vars/we"), Some("ird.yml"));
        assert_eq!(p.raw_params, None);
    }

    #[test]
    fn quoting_does_not_protect_an_equals() {
        // Live-verified: `"'we=ird.yml'"` errors as option `'we`.
        let p = kv("'we=ird.yml'");
        assert_eq!(p.get("'we"), Some("ird.yml'"));
    }

    #[test]
    fn duplicate_keys_last_wins() {
        assert_eq!(kv("a=1 a=2").get("a"), Some("2"));
    }

    #[test]
    fn unbalanced_quote_is_an_error() {
        assert!(parse_kv(r#"a="unclosed"#, false).is_err());
        assert!(parse_kv("x={{ open", false).is_err());
    }

    #[test]
    fn escapes_decode_in_values() {
        assert_eq!(kv(r"msg=a\tb").get("msg"), Some("a\tb"));
    }

    /// Ansible's own suite, ported verbatim: `test/units/parsing/test_splitter.py`
    /// `SPLIT_DATA` (identical in 2.21.2 and 2.22.0.dev0). Each row: input, expected
    /// `split_args`, expected options, expected `_raw_params`.
    #[rustfmt::skip]
    const SPLIT_DATA: &[(&str, &[&str], &[(&str, &str)], Option<&str>)] = &[
        ("", &[], &[], None),
        ("a", &["a"], &[], Some("a")),
        ("a=b", &["a=b"], &[("a", "b")], None),
        ("a=\"foo bar\"", &["a=\"foo bar\""], &[("a", "foo bar")], None),
        ("\"foo bar baz\"", &["\"foo bar baz\""], &[], Some("\"foo bar baz\"")),
        ("foo bar baz", &["foo", "bar", "baz"], &[], Some("foo bar baz")),
        ("a=b c=\"foo bar\"", &["a=b", "c=\"foo bar\""], &[("a", "b"), ("c", "foo bar")], None),
        ("a=\"echo \\\"hello world\\\"\" b=bar",
            &["a=\"echo \\\"hello world\\\"\"", "b=bar"],
            &[("a", "echo \"hello world\""), ("b", "bar")], None),
        ("a=\"nest'ed\"", &["a=\"nest'ed\""], &[("a", "nest'ed")], None),
        (" ", &[" "], &[], Some(" ")),
        ("\\ ", &[" "], &[], Some(" ")),
        ("a\\=escaped", &["a\\=escaped"], &[], Some("a=escaped")),
        ("a=\"multi\nline\"", &["a=\"multi\nline\""], &[("a", "multi\nline")], None),
        ("a=\"blank\n\nline\"", &["a=\"blank\n\nline\""], &[("a", "blank\n\nline")], None),
        ("a=\"blank\n\n\nlines\"", &["a=\"blank\n\n\nlines\""], &[("a", "blank\n\n\nlines")], None),
        ("a=\"a long\nmessage\\\nabout a thing\n\"",
            &["a=\"a long\nmessage\\\nabout a thing\n\""],
            &[("a", "a long\nmessage\\\nabout a thing\n")], None),
        ("a=\"multiline\nmessage1\\\n\" b=\"multiline\nmessage2\\\n\"",
            &["a=\"multiline\nmessage1\\\n\"", "b=\"multiline\nmessage2\\\n\""],
            &[("a", "multiline\nmessage1\\\n"), ("b", "multiline\nmessage2\\\n")], None),
        ("line \\\ncontinuation", &["line", "continuation"], &[], Some("line continuation")),
        ("not jinja}}", &["not", "jinja}}"], &[], Some("not jinja}}")),
        ("a={{multiline\njinja}}", &["a={{multiline\njinja}}"], &[("a", "{{multiline\njinja}}")], None),
        ("a={{jinja}}", &["a={{jinja}}"], &[("a", "{{jinja}}")], None),
        ("a={{ jinja }}", &["a={{ jinja }}"], &[("a", "{{ jinja }}")], None),
        ("a={% jinja %}", &["a={% jinja %}"], &[("a", "{% jinja %}")], None),
        ("a={# jinja #}", &["a={# jinja #}"], &[("a", "{# jinja #}")], None),
        ("a=\"{{jinja}}\"", &["a=\"{{jinja}}\""], &[("a", "{{jinja}}")], None),
        ("a={{ jinja }}{{jinja2}}", &["a={{ jinja }}{{jinja2}}"], &[("a", "{{ jinja }}{{jinja2}}")], None),
        ("a=\"{{ jinja }}{{jinja2}}\"", &["a=\"{{ jinja }}{{jinja2}}\""], &[("a", "{{ jinja }}{{jinja2}}")], None),
        ("a={{jinja}} b={{jinja2}}", &["a={{jinja}}", "b={{jinja2}}"], &[("a", "{{jinja}}"), ("b", "{{jinja2}}")], None),
        ("a=\"{{jinja}}\n\" b=\"{{jinja2}}\n\"",
            &["a=\"{{jinja}}\n\"", "b=\"{{jinja2}}\n\""],
            &[("a", "{{jinja}}\n"), ("b", "{{jinja2}}\n")], None),
        ("a=\"café eñyei\"", &["a=\"café eñyei\""], &[("a", "café eñyei")], None),
        ("a=café b=eñyei", &["a=café", "b=eñyei"], &[("a", "café"), ("b", "eñyei")], None),
        ("a={{ foo | some_filter(' ', \" \") }} b=bar",
            &["a={{ foo | some_filter(' ', \" \") }}", "b=bar"],
            &[("a", "{{ foo | some_filter(' ', \" \") }}"), ("b", "bar")], None),
        ("One\n  Two\n    Three\n",
            &["One\n ", "Two\n   ", "Three\n"],
            &[], Some("One\n  Two\n    Three\n")),
        ("\nOne\n  Two\n    Three\n",
            &["\n", "One\n ", "Two\n   ", "Three\n"],
            &[], Some("\nOne\n  Two\n    Three\n")),
    ];

    #[test]
    fn ansible_upstream_split_data() {
        for (input, split, options, raw) in SPLIT_DATA {
            assert_eq!(&split_args(input).unwrap(), split, "split_args({input:?})");
            let p = parse_kv(input, false).unwrap();
            let want: Vec<(String, String)> =
                options.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            assert_eq!(p.options, want, "parse_kv({input:?}) options");
            assert_eq!(p.raw_params.as_deref(), *raw, "parse_kv({input:?}) raw");
        }
    }

    #[test]
    fn ansible_upstream_check_raw_table() {
        let p = parse_kv("raw=yes", true).unwrap();
        assert_eq!(p.raw_params.as_deref(), Some("raw=yes"));
        assert!(p.options.is_empty());
        let p = parse_kv("creates=something", true).unwrap();
        assert_eq!(p.get("creates"), Some("something"));
        assert_eq!(p.raw_params, None);
    }

    #[test]
    fn ansible_upstream_parser_errors() {
        for input in ["\"", "'", "{{", "{%", "{#"] {
            assert!(split_args(input).is_err(), "split_args({input:?}) should error");
            assert!(parse_kv(input, false).is_err(), "parse_kv({input:?}) should error");
        }
    }
}
