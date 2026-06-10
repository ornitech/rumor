use std::collections::HashMap;

use anyhow::{anyhow, Result};

/// Substitute `${VAR}` references in `input` against `env`.
///
/// - `${NAME}` where `NAME` matches `[A-Za-z_][A-Za-z0-9_]*` -> the value of
///   `NAME` in `env`.
/// - `$$` -> literal `$`.
/// - Missing variable: emits an empty string and logs a `tracing::warn!`,
///   tagged with `context` so the user can locate the offending field.
/// - A bare `$` not followed by `{` or `$` is emitted literally.
/// - `${...}` whose contents do not match the strict identifier shape (e.g.
///   `${RATE:-1}` or `${FOO BAR}`) is passed through verbatim. This lets users
///   write shell-side interpolation in `command`/`args` without escaping.
/// - Errors only on truly malformed templates: an unterminated `${...` with
///   no closing `}`.
pub fn substitute(input: &str, env: &HashMap<String, String>, context: &str) -> Result<String> {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b != b'$' {
            // Find the next `$` (or end) and copy that slice verbatim. This
            // preserves multi-byte UTF-8 sequences without parsing them.
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != b'$' {
                j += 1;
            }
            out.push_str(&input[i..j]);
            i = j;
            continue;
        }

        // We saw a `$`. Look ahead.
        let next = bytes.get(i + 1).copied();
        match next {
            Some(b'$') => {
                out.push('$');
                i += 2;
            }
            Some(b'{') => {
                // Find the closing `}`. Required, even when we're going to
                // pass through verbatim (so we can detect truly broken input).
                let name_start = i + 2;
                let close = bytes[name_start..]
                    .iter()
                    .position(|&c| c == b'}')
                    .map(|off| name_start + off)
                    .ok_or_else(|| {
                        anyhow!(
                            "{context}: invalid template: unterminated `${{...` (missing `}}`)"
                        )
                    })?;
                let inner = &input[name_start..close];
                if is_strict_ident(inner) {
                    match env.get(inner) {
                        Some(v) => out.push_str(v),
                        None => {
                            tracing::warn!(
                                "{context}: env var ${{{inner}}} is not set; substituting empty string"
                            );
                        }
                    }
                } else {
                    // Not a rumor template; pass the whole `${...}` through
                    // verbatim so downstream shells can interpolate it.
                    out.push_str(&input[i..=close]);
                }
                i = close + 1;
            }
            _ => {
                // Lone `$` (end of string or followed by something else): emit literal.
                out.push('$');
                i += 1;
            }
        }
    }
    Ok(out)
}

pub(crate) fn is_strict_ident(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    let first = bytes[0];
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|&c| c.is_ascii_alphanumeric() || c == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect()
    }

    #[test]
    fn substitutes_simple_var() {
        let e = env(&[("PORT", "5432")]);
        assert_eq!(substitute("${PORT}", &e, "ctx").unwrap(), "5432");
    }

    #[test]
    fn substitutes_in_middle() {
        let e = env(&[("X", "abc")]);
        assert_eq!(substitute("a-${X}-b", &e, "ctx").unwrap(), "a-abc-b");
    }

    #[test]
    fn missing_var_becomes_empty() {
        let e = env(&[]);
        assert_eq!(substitute("a-${MISSING}-b", &e, "ctx").unwrap(), "a--b");
    }

    #[test]
    fn double_dollar_escapes() {
        let e = env(&[("X", "y")]);
        assert_eq!(substitute("$${X}", &e, "ctx").unwrap(), "${X}");
    }

    #[test]
    fn lone_dollar_is_literal() {
        let e = env(&[]);
        assert_eq!(substitute("price: $5", &e, "ctx").unwrap(), "price: $5");
        assert_eq!(substitute("trailing $", &e, "ctx").unwrap(), "trailing $");
    }

    #[test]
    fn unterminated_errors() {
        let e = env(&[]);
        let err = substitute("${FOO", &e, "ctx").unwrap_err().to_string();
        assert!(err.contains("unterminated"), "got: {err}");
        assert!(err.contains("ctx"), "got: {err}");
    }

    #[test]
    fn empty_braces_pass_through() {
        let e = env(&[]);
        assert_eq!(substitute("${}", &e, "ctx").unwrap(), "${}");
    }

    #[test]
    fn invalid_identifier_passes_through() {
        // `${1FOO}` doesn't match our strict identifier shape, so it survives
        // verbatim instead of erroring.
        let e = env(&[]);
        assert_eq!(substitute("${1FOO}", &e, "ctx").unwrap(), "${1FOO}");
    }

    #[test]
    fn shell_default_value_syntax_passes_through() {
        // `${RATE:-1}` is bash's default-value syntax. Rumor should not eat it.
        let e = env(&[]);
        assert_eq!(
            substitute("sleep ${RATE:-1}", &e, "ctx").unwrap(),
            "sleep ${RATE:-1}"
        );
    }

    #[test]
    fn preserves_utf8() {
        let e = env(&[("X", "world")]);
        assert_eq!(substitute("héllo ${X}", &e, "ctx").unwrap(), "héllo world");
    }

    #[test]
    fn empty_input() {
        let e = env(&[]);
        assert_eq!(substitute("", &e, "ctx").unwrap(), "");
    }

    #[test]
    fn multiple_substitutions() {
        let e = env(&[("A", "1"), ("B", "2")]);
        assert_eq!(substitute("${A}+${B}=${A}${B}", &e, "ctx").unwrap(), "1+2=12");
    }
}
