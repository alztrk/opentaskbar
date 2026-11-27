//! Lenient JSON helpers shared by icon-pack and theme-pack manifests.
//!
//! The reference serializers accepted comments and trailing commas, and matched
//! manifest keys case-insensitively. Keeping the parsing in one place ensures
//! both manifest readers apply exactly the same relaxations.

use serde_json::{Map, Value};

/// Finds a key in a JSON object, ignoring ASCII case.
pub fn object_get_ci<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    object
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(key))
        .map(|(_, value)| value)
}

/// Looks up a case-insensitive key in a JSON object value.
pub fn json_get_ci<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value
        .as_object()
        .and_then(|object| object_get_ci(object, key))
}

/// Reads a trimmed string field, or an empty string when missing/not a string.
pub fn string_ci(value: &Value, key: &str) -> String {
    json_get_ci(value, key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Strips `//` and `/* */` comments and trailing commas, leaving string
/// contents (including URLs such as `https://`) untouched.
pub fn relaxed_json(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    let mut in_string = false;
    let mut escaped = false;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(c);
            i += 1;
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            i += 2;
            while i < chars.len() && chars[i] != '\n' && chars[i] != '\r' {
                i += 1;
            }
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(chars.len());
            continue;
        }
        if c == ','
            && let Some(next) = chars[i + 1..].iter().find(|next| !next.is_whitespace())
            && (*next == '}' || *next == ']')
        {
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relaxed_json_keeps_urls_and_strips_comments_and_trailing_commas() {
        let input = r#"{
            // line comment
            "url": "https://example.com/a//b", /* block */
            "name": "a,b",
        }"#;
        assert_eq!(
            relaxed_json(input),
            "{\n            \n            \"url\": \"https://example.com/a//b\", \n            \"name\": \"a,b\"\n        }"
        );
    }

    #[test]
    fn case_insensitive_lookup_reads_trimmed_strings() {
        let value: Value = serde_json::from_str(r#"{"Name":"  Nord  ","n":7}"#).unwrap();
        assert_eq!(string_ci(&value, "NAME"), "Nord");
        assert_eq!(json_get_ci(&value, "n").and_then(Value::as_i64), Some(7));
        assert_eq!(string_ci(&value, "missing"), "");
    }

    #[test]
    fn relaxed_json_parses_after_transformation() {
        let text = "{/*c*/\"Name\":\"x\",}";
        let value: Value = serde_json::from_str(&relaxed_json(text)).unwrap();
        assert_eq!(string_ci(&value, "name"), "x");
    }
}
