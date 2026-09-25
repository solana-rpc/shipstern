//! Port of `@codama/nodes` `stringCases.ts`. `CamelCaseString::new` disagrees
//! with it on acronyms and digits (`USDC` -> `usdc`, JS gives `uSDC`), so names
//! are cased here and wrapped without re-normalizing.

use codama_nodes::CamelCaseString;

use crate::Error;

///
/// JS `camelCase`, wrapped verbatim.
///
/// Example output:
///
/// ```rust, ignore
/// assert_eq!(camel("amount_in")?.as_str(), "amountIn");
/// assert_eq!(camel("USDC")?.as_str(), "uSDC");
/// ```
///
pub(crate) fn camel(input: &str) -> Result<CamelCaseString, Error> {
    let cased = camel_case(input);

    // Deserialization is the one constructor that keeps the string verbatim.
    Ok(serde_json::from_value(serde_json::Value::String(cased))?)
}

pub(crate) fn camel_case(input: &str) -> String {
    let pascal = pascal_case(input);

    let mut chars = pascal.chars();

    match chars.next() {
        Some(first) => first.to_ascii_lowercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

fn pascal_case(input: &str) -> String { title_words(input).concat() }

/// The capitalized words of JS `titleCase`. Only ASCII alphanumerics count as
/// word characters, matching its `[^a-zA-Z0-9]` split.
fn title_words(input: &str) -> Vec<String> {
    if is_screaming_snake(input) {
        return input
            .to_ascii_lowercase()
            .split('_')
            .map(capitalize)
            .collect();
    }

    let mut spaced = String::with_capacity(input.len() * 2);

    for c in input.chars() {
        if c.is_ascii_uppercase() {
            spaced.push(' ');
        }

        spaced.push(c);
    }

    spaced
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(capitalize)
        .collect()
}

/// JS's `^[A-Z][A-Z0-9]*(?:_[A-Z0-9]+)+$`.
fn is_screaming_snake(input: &str) -> bool {
    let mut parts = input.split('_');

    let Some(head) = parts.next() else {
        return false;
    };

    let upper_or_digit = |c: char| c.is_ascii_uppercase() || c.is_ascii_digit();

    let head_ok =
        head.starts_with(|c: char| c.is_ascii_uppercase()) && head.chars().all(upper_or_digit);

    let rest: Vec<&str> = parts.collect();

    head_ok
        && !rest.is_empty()
        && rest
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(upper_or_digit))
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();

    match chars.next() {
        Some(first) => {
            first.to_ascii_uppercase().to_string() + &chars.as_str().to_ascii_lowercase()
        },
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::camel_case;

    /// Expected values were produced by `camelCase` from `@codama/nodes@1.11.0`.
    #[test]
    fn matches_the_js_implementation() {
        let cases = [
            ("amount_in", "amountIn"),
            ("minAmountOut", "minAmountOut"),
            ("SwapResult", "swapResult"),
            ("USDC", "uSDC"),
            ("MAX_FEE_BPS", "maxFeeBps"),
            ("str1ng", "str1ng"),
            ("v2_pool", "v2Pool"),
            ("buyExactQuoteInV2", "buyExactQuoteInV2"),
            ("__leading", "leading"),
            ("", ""),
        ];

        for (input, expected) in cases {
            assert_eq!(camel_case(input), expected, "camelCase({input:?})");
        }
    }
}
