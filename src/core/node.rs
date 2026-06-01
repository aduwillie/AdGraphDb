// Node — the fundamental vertex of the graph.
//
// A node carries:
//   • a stable, unique numeric identity (NodeId)
//   • a human-readable label (e.g. "Person", "City")
//   • an open property bag for arbitrary key/value metadata

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::value::Value;

// ── NodeId ────────────────────────────────────────────────────────────────────
//
// Newtype so the compiler distinguishes node IDs from edge IDs and raw u64s.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub u64);

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "N{}", self.0)
    }
}

// ── Node ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub label: String,
    pub properties: HashMap<String, Value>,
}

impl Node {
    pub fn new(id: NodeId, label: impl Into<String>) -> Self {
        Self {
            id,
            label: label.into(),
            properties: HashMap::new(),
        }
    }

    pub fn with_property(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.properties.insert(key.into(), value.into());
        self
    }
}

impl std::fmt::Display for Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "({} :{})", self.id, self.label)?;
        if !self.properties.is_empty() {
            write!(f, " {{")?;
            for (i, (k, v)) in self.properties.iter().enumerate() {
                if i > 0 { write!(f, ", ")?; }
                write!(f, "{k}: {v}")?;
            }
            write!(f, "}}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_id_display() {
        assert_eq!(NodeId(0).to_string(), "N0");
        assert_eq!(NodeId(99).to_string(), "N99");
    }

    #[test]
    fn node_id_equality_and_hash() {
        use std::collections::HashSet;
        let a = NodeId(1);
        let b = NodeId(1);
        let c = NodeId(2);
        assert_eq!(a, b);
        assert_ne!(a, c);
        let mut set = HashSet::new();
        set.insert(a);
        assert!(set.contains(&b));
        assert!(!set.contains(&c));
    }

    #[test]
    fn new_node_has_no_properties() {
        let node = Node::new(NodeId(0), "Person");
        assert_eq!(node.label, "Person");
        assert!(node.properties.is_empty());
    }

    #[test]
    fn with_property_builder() {
        let node = Node::new(NodeId(0), "Person")
            .with_property("name", "Alice")
            .with_property("age", 30_i64);
        assert_eq!(node.properties["name"], Value::Text("Alice".into()));
        assert_eq!(node.properties["age"], Value::Integer(30));
    }

    #[test]
    fn display_no_properties() {
        let node = Node::new(NodeId(5), "City");
        let s = node.to_string();
        assert!(s.contains("N5"));
        assert!(s.contains("City"));
    }

    #[test]
    fn display_with_properties_contains_key() {
        let node = Node::new(NodeId(1), "City")
            .with_property("name", "London");
        assert!(node.to_string().contains("name"));
    }

    #[test]
    fn clone_is_independent() {
        let original = Node::new(NodeId(0), "X")
            .with_property("k", "v");
        let mut cloned = original.clone();
        cloned.properties.insert("extra".into(), Value::Integer(1));
        assert!(!original.properties.contains_key("extra"));
    }
}
