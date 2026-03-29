use std::collections::HashMap;

/// A template field value. Missing fields (Any) match anything.
#[derive(Debug, Clone)]
pub enum MatchOp {
    Eq(String),
    Contains(String),
    StartsWith(String),
    Any,
}

impl MatchOp {
    pub fn matches(&self, value: &str) -> bool {
        match self {
            MatchOp::Eq(expected) => value == expected,
            MatchOp::Contains(sub) => value.contains(sub.as_str()),
            MatchOp::StartsWith(prefix) => value.starts_with(prefix.as_str()),
            MatchOp::Any => true,
        }
    }
}

/// Parse a template map into MatchOps.
///
/// Template values can be plain strings (Eq), or structured operators:
///   "contains:foo"    → MatchOp::Contains("foo")
///   "starts_with:foo" → MatchOp::StartsWith("foo")
///   "" or "*"         → MatchOp::Any
///   anything else     → MatchOp::Eq(value)
pub fn parse_template(template: &HashMap<String, String>) -> HashMap<String, MatchOp> {
    template
        .iter()
        .map(|(k, v)| {
            let op = if v.is_empty() || v == "*" {
                MatchOp::Any
            } else if let Some(rest) = v.strip_prefix("contains:") {
                MatchOp::Contains(rest.to_string())
            } else if let Some(rest) = v.strip_prefix("starts_with:") {
                MatchOp::StartsWith(rest.to_string())
            } else {
                MatchOp::Eq(v.clone())
            };
            (k.clone(), op)
        })
        .collect()
}

/// Returns true if `attrs` satisfies all fields in `template`.
/// Fields present in the template but absent in attrs are a non-match
/// (unless the op is Any).
pub fn matches(template: &HashMap<String, MatchOp>, attrs: &HashMap<String, String>) -> bool {
    template.iter().all(|(key, op)| match attrs.get(key) {
        Some(val) => op.matches(val),
        None => matches!(op, MatchOp::Any),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attrs(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn template(pairs: &[(&str, &str)]) -> HashMap<String, MatchOp> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), MatchOp::Eq(v.to_string())))
            .collect()
    }

    #[test]
    fn exact_match() {
        let t = template(&[("interface", "WeatherStation"), ("region", "tampa-east")]);
        let a = attrs(&[
            ("interface", "WeatherStation"),
            ("region", "tampa-east"),
            ("station", "7"),
        ]);
        assert!(matches(&t, &a));
    }

    #[test]
    fn missing_field_is_nonmatch() {
        let t = template(&[("interface", "WeatherStation"), ("region", "miami")]);
        let a = attrs(&[("interface", "WeatherStation"), ("region", "tampa-east")]);
        assert!(!matches(&t, &a));
    }

    #[test]
    fn any_matches_present_field() {
        let mut t = template(&[("interface", "WeatherStation")]);
        t.insert("region".to_string(), MatchOp::Any);
        let a = attrs(&[("interface", "WeatherStation"), ("region", "anything")]);
        assert!(matches(&t, &a));
    }

    #[test]
    fn contains_match() {
        let mut t = HashMap::new();
        t.insert(
            "metrics".to_string(),
            MatchOp::Contains("humidity".to_string()),
        );
        let a = attrs(&[("metrics", "wind,humidity,temp")]);
        assert!(matches(&t, &a));
    }
}
