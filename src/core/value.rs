// Value — the property value type stored on nodes and edges.
//
// Kept deliberately small so the binary codec stays readable.
// Extend by adding new variants and updating the codec in
// adapters/storage/binary_file.rs.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Null,
    Boolean(bool),
    Integer(i64),
    Float(f64),
    Text(String),
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Null => write!(f, "null"),
            Value::Boolean(b) => write!(f, "{b}"),
            Value::Integer(i) => write!(f, "{i}"),
            Value::Float(fl) => write!(f, "{fl}"),
            Value::Text(s) => write!(f, "\"{s}\""),
        }
    }
}

// ── Convenience From impls ────────────────────────────────────────────────────

impl From<bool> for Value {
    fn from(v: bool) -> Self { Value::Boolean(v) }
}

impl From<i64> for Value {
    fn from(v: i64) -> Self { Value::Integer(v) }
}

impl From<f64> for Value {
    fn from(v: f64) -> Self { Value::Float(v) }
}

impl From<&str> for Value {
    fn from(v: &str) -> Self { Value::Text(v.to_owned()) }
}

impl From<String> for Value {
    fn from(v: String) -> Self { Value::Text(v) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_null() {
        assert_eq!(Value::Null.to_string(), "null");
    }

    #[test]
    fn display_boolean() {
        assert_eq!(Value::Boolean(true).to_string(), "true");
        assert_eq!(Value::Boolean(false).to_string(), "false");
    }

    #[test]
    fn display_integer() {
        assert_eq!(Value::Integer(42).to_string(), "42");
        assert_eq!(Value::Integer(-7).to_string(), "-7");
    }

    #[test]
    fn display_float() {
        assert_eq!(Value::Float(3.14).to_string(), "3.14");
    }

    #[test]
    fn display_text_includes_quotes() {
        assert_eq!(Value::Text("hello".into()).to_string(), "\"hello\"");
    }

    #[test]
    fn from_bool() {
        assert_eq!(Value::from(true), Value::Boolean(true));
    }

    #[test]
    fn from_i64() {
        assert_eq!(Value::from(100_i64), Value::Integer(100));
    }

    #[test]
    fn from_f64() {
        assert_eq!(Value::from(2.0_f64), Value::Float(2.0));
    }

    #[test]
    fn from_str_ref() {
        assert_eq!(Value::from("world"), Value::Text("world".into()));
    }

    #[test]
    fn from_string() {
        assert_eq!(Value::from("owned".to_string()), Value::Text("owned".into()));
    }

    #[test]
    fn equality_across_same_variants() {
        assert_eq!(Value::Integer(5), Value::Integer(5));
        assert_ne!(Value::Integer(5), Value::Integer(6));
        assert_ne!(Value::Integer(5), Value::Float(5.0));
    }

    #[test]
    fn clone_preserves_value() {
        let original = Value::Text("clone me".into());
        let cloned = original.clone();
        assert_eq!(original, cloned);
    }
}
