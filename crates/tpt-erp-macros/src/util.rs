//! Small string helpers shared by the derives.

/// Convert `CamelCase` / `PascalCase` to `snake_case`.
///
/// Handles acronyms correctly: `URL` -> `url`, `URLShort` -> `url_short`,
/// `OrderLineItem` -> `order_line_item`, `HTTPServer` -> `http_server`.
pub(crate) fn to_snake_case(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c.is_uppercase() {
            let prev_lower = i > 0 && chars[i - 1].is_lowercase();
            let prev_upper = i > 0 && chars[i - 1].is_uppercase();
            let next_lower = chars.get(i + 1).map_or(false, |&n| n.is_lowercase());
            if i > 0 && (prev_lower || (prev_upper && next_lower)) {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake() {
        assert_eq!(to_snake_case("Customer"), "customer");
        assert_eq!(to_snake_case("OrderLineItem"), "order_line_item");
        assert_eq!(to_snake_case("URL"), "url");
        assert_eq!(to_snake_case("URLShort"), "url_short");
        assert_eq!(to_snake_case("HTTPServer"), "http_server");
    }
}
