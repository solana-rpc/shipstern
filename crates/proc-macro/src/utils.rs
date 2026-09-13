/// Left-pad a hex string with a leading zero if it has an odd number of characters.
/// `hex::decode` requires even-length input (each byte = 2 hex chars).
pub fn pad_hex(hex: &str) -> String {
    if hex.len().is_multiple_of(2) {
        hex.to_string()
    } else {
        format!("0{hex}")
    }
}

pub fn decode_hex_text(value: &str) -> Result<Vec<u8>, hex::FromHexError> {
    let trimmed = value.trim();
    let without_prefix = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    let cleaned: String = without_prefix
        .chars()
        .filter(|c| !c.is_ascii_whitespace() && *c != '_')
        .collect();

    hex::decode(pad_hex(&cleaned))
}

///
/// Convert a PascalCase or camelCase string to snake_case.
///
/// Handles consecutive uppercase (acronyms) correctly:
/// - `HTTPServer` → `http_server`
/// - `OpenDcaV2` → `open_dca_v2`
/// - `BorrowFromCustody` → `borrow_from_custody`
///
pub fn to_snake_case(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 4);

    for (i, &c) in chars.iter().enumerate() {
        if c.is_uppercase() && i > 0 {
            let prev_upper = chars[i - 1].is_uppercase();
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());

            // Insert underscore when:
            // - previous char was lowercase (e.g. the S in "FromServer")
            // - OR previous was uppercase but next is lowercase (e.g. the S in "HTTPServer")
            if !prev_upper || next_lower {
                out.push('_');
            }
        }

        out.push(c.to_ascii_lowercase());
    }

    out
}

pub fn to_pascal_case(str: &str) -> String {
    let mut result = String::with_capacity(str.len());
    let mut capitalize_next = true;

    for ch in str.chars() {
        if ch == '_' {
            capitalize_next = true;
        } else if capitalize_next {
            result.push(ch.to_ascii_uppercase());
            capitalize_next = false;
        } else {
            result.push(ch);
        }
    }

    result
}

///
/// Render a count, size or offset as an **unsuffixed** integer literal.
///
/// `quote!` interpolates a bare integer as a *suffixed* literal: a `usize`
/// becomes `558usize`, a `u64` becomes `558u64`. That pins the generated code
/// to the width codama happens to use for that field. Generated parsers compare
/// against `data.len()` and declare `usize` consts, so the day codama widens a
/// count/size/offset the emitted code stops compiling.
///
/// An unsuffixed literal lets inference pick the width at the use site, which
/// keeps the generated code correct on both sides of that change.
///
/// Example output:
///
/// ```rust, ignore
/// // suffixed (what bare interpolation emits once the field is u64):
/// data.len() == 558u64   // error[E0308]: expected `usize`, found `u64`
///
/// // unsuffixed (what this emits):
/// data.len() == 558      // infers usize
/// ```
///
pub(crate) fn unsuffixed(value: u64) -> proc_macro2::Literal {
    proc_macro2::Literal::u64_unsuffixed(value)
}
