//! Small string helpers shared by the derives.

/// Convert `CamelCase` / `PascalCase` to `snake_case`.
pub(crate) fn to_snake_case(s: &str) -> String {
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() {
            if i != 0 {
                out.push('_');
            }
            for c in ch.to_lowercase() {
                out.push(c);
            }
        } else {
            out.push(ch);
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
        assert_eq!(to_snake_case("URL"), "u_r_l");
    }
}
