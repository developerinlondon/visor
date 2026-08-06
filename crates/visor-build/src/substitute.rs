//! ARG/ENV variable substitution for Dockerfile strings.
//!
//! Supports `${VAR}`, `$VAR`, `${VAR:-default}`, `${VAR:+alt}`, and `$$`
//! (escaped literal dollar) syntax. Unknown variables resolve to the empty
//! string, matching Docker's behaviour.

use std::collections::HashMap;
use std::hash::BuildHasher;

/// Perform Dockerfile-style variable substitution on `input`.
///
/// # Supported syntax
///
/// | Pattern              | Meaning                                       |
/// |----------------------|-----------------------------------------------|
/// | `$VAR`               | Simple substitution                           |
/// | `${VAR}`             | Braced substitution                           |
/// | `${VAR:-default}`    | Use *default* when VAR is unset **or empty**  |
/// | `${VAR:+alt}`        | Use *alt* only when VAR **is** set and non-empty |
/// | `$$`                 | Literal `$`                                   |
///
/// # Errors
///
/// Returns an error when braced variable syntax is malformed (e.g. missing
/// closing `}`).
pub fn substitute_vars<S: BuildHasher>(
    input: &str,
    vars: &HashMap<String, String, S>,
) -> anyhow::Result<String> {
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len);
    let mut i = 0;

    while i < len {
        if bytes[i] != b'$' {
            // SAFETY: i < len guarantees the slice is non-empty, so chars().next()
            // always returns Some.
            let ch = input[i..].chars().next().unwrap_or('\0');
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }

        // We're at a '$'
        // Check for '$$' escape
        if i + 1 < len && bytes[i + 1] == b'$' {
            out.push('$');
            i += 2;
            continue;
        }

        // Check for '${...}' braced form
        if i + 1 < len && bytes[i + 1] == b'{' {
            let start = i + 2;
            let close = find_closing_brace(input, start)?;
            let inner = &input[start..close];
            expand_braced(inner, vars, &mut out);
            i = close + 1;
            continue;
        }

        // Check for '$VAR' simple form — var names are [A-Za-z0-9_]
        if i + 1 < len && is_var_start(bytes[i + 1]) {
            let start = i + 1;
            let end = var_name_end(input, start);
            let name = &input[start..end];
            if let Some(val) = vars.get(name) {
                out.push_str(val);
            }
            // Unknown → empty string (pushed nothing)
            i = end;
            continue;
        }

        // Lone '$' at end or followed by non-identifier char → literal '$'
        out.push('$');
        i += 1;
    }

    Ok(out)
}

/// Find the matching `}` for a braced expression starting at `start`.
fn find_closing_brace(input: &str, start: usize) -> anyhow::Result<usize> {
    let bytes = input.as_bytes();
    let mut depth: u32 = 1;
    let mut j = start;
    while j < bytes.len() {
        match bytes[j] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(j);
                }
            }
            _ => {}
        }
        j += 1;
    }
    anyhow::bail!("unclosed '${{' in variable expression starting near position {start}")
}

/// Expand a braced expression (the content between `${` and `}`).
///
/// Handles: `VAR`, `VAR:-default`, `VAR:+alt`.
fn expand_braced<S: BuildHasher>(inner: &str, vars: &HashMap<String, String, S>, out: &mut String) {
    // Check for ':-' (default value)
    if let Some(pos) = inner.find(":-") {
        let name = &inner[..pos];
        let default = &inner[pos + 2..];
        match vars.get(name) {
            Some(val) if !val.is_empty() => out.push_str(val),
            _ => out.push_str(default),
        }
        return;
    }

    // Check for ':+' (alternative value)
    if let Some(pos) = inner.find(":+") {
        let name = &inner[..pos];
        let alt = &inner[pos + 2..];
        if let Some(val) = vars.get(name) {
            if !val.is_empty() {
                out.push_str(alt);
            }
        }
        // unset or empty → push nothing
        return;
    }

    // Plain braced: ${VAR}
    if let Some(val) = vars.get(inner) {
        out.push_str(val);
    }
}

/// Returns `true` if `b` can start a variable name (`[A-Za-z_]`).
const fn is_var_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

/// Returns `true` if `b` can continue a variable name (`[A-Za-z0-9_]`).
const fn is_var_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Return the index just past the end of a variable name starting at `start`.
fn var_name_end(input: &str, start: usize) -> usize {
    let bytes = input.as_bytes();
    let mut end = start;
    while end < bytes.len() && is_var_char(bytes[end]) {
        end += 1;
    }
    end
}

#[cfg(test)]
#[path = "substitute_test.rs"]
mod tests;
