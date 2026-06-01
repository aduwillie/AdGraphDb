// Edge — a directed, weighted relationship between two nodes.
//
// Direction matters: (source) ──label──> (target).
// Undirected graphs can be modelled by inserting two edges in opposite
// directions, or by treating the engine as undirected at query time.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::{node::NodeId, value::Value};

// ── EdgeId ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EdgeId(pub u64);

impl std::fmt::Display for EdgeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "E{}", self.0)
    }
}

// ── Edge ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub id: EdgeId,
    pub source: NodeId,
    pub target: NodeId,
    pub label: String,
    /// Numeric weight used by shortest-path algorithms.
    /// Defaults to 1.0 for unweighted graphs.
    pub weight: f64,
    pub properties: HashMap<String, Value>,
}

impl Edge {
    pub fn new(
        id: EdgeId,
        source: NodeId,
        target: NodeId,
        label: impl Into<String>,
        weight: f64,
    ) -> Self {
        Self {
            id,
            source,
            target,
            label: label.into(),
            weight,
            properties: HashMap::new(),
        }
    }

    pub fn with_property(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.properties.insert(key.into(), value.into());
        self
    }
}

impl std::fmt::Display for Edge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "({}) -[{} :{}]-> ({})",
            self.source, self.id, self.label, self.target
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src() -> NodeId { NodeId(0) }
    fn tgt() -> NodeId { NodeId(1) }

    #[test]
    fn edge_id_display() {
        assert_eq!(EdgeId(0).to_string(), "E0");
        assert_eq!(EdgeId(42).to_string(), "E42");
    }

    #[test]
    fn new_edge_stores_all_fields() {
        let e = Edge::new(EdgeId(0), src(), tgt(), "KNOWS", 2.5);
        assert_eq!(e.source, src());
        assert_eq!(e.target, tgt());
        assert_eq!(e.label, "KNOWS");
        assert!((e.weight - 2.5).abs() < f64::EPSILON);
        assert!(e.properties.is_empty());
    }

    #[test]
    fn with_property_builder() {
        let e = Edge::new(EdgeId(0), src(), tgt(), "KNOWS", 1.0)
            .with_property("since", 2020_i64);
        assert_eq!(e.properties["since"], Value::Integer(2020));
    }

    #[test]
    fn display_contains_source_and_target() {
        let e = Edge::new(EdgeId(3), src(), tgt(), "RAIL", 1.0);
        let s = e.to_string();
        assert!(s.contains("N0"));
        assert!(s.contains("N1"));
        assert!(s.contains("RAIL"));
        assert!(s.contains("E3"));
    }

    #[test]
    fn edge_id_equality() {
        assert_eq!(EdgeId(5), EdgeId(5));
        assert_ne!(EdgeId(5), EdgeId(6));
    }
}
