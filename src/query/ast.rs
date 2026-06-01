// QueryCommand — the shared intermediate representation (IR) produced by every parser.
//
// This is the Command pattern applied to graph queries.
//
//   ┌──────────────┐         ┌──────────────┐
//   │ SimpleQuery  │ parse   │              │ execute
//   │   Parser     │────────>│ QueryCommand │──────────> DatabaseContext
//   └──────────────┘         │     (IR)     │
//   ┌──────────────┐         │              │
//   │ CypherLite   │ parse   │              │
//   │   Parser     │────────>└──────────────┘
//   └──────────────┘
//
// Both parsers produce this same IR.  The executor (query/executor.rs) runs it
// against any DatabaseContext without knowing which parser produced it.
//
// Adding a new query language means implementing a new parser that emits
// QueryCommand values — the executor and database are untouched.

use crate::core::{edge::EdgeId, node::NodeId, value::Value};

// ── Top-level command ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum QueryCommand {
    MatchNodes(NodeFilter),
    MatchEdges(EdgeFilter),
    GetNode(NodeId),
    GetEdge(EdgeId),
    Traverse { kind: TraversalKind, start: NodeId },
    ShortestPath { start: NodeId, goal: NodeId },
    CountNodes,
    CountEdges,
}

// ── Traversal kind ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum TraversalKind {
    Bfs,
    Dfs,
}

// ── Node filter ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct NodeFilter {
    /// If Some, only nodes whose `label` field matches are returned.
    pub label: Option<String>,
    /// All conditions must hold (implicit AND).
    pub property_conditions: Vec<PropertyCondition>,
}

impl NodeFilter {
    pub fn matches(&self, node: &crate::core::node::Node) -> bool {
        if let Some(ref expected) = self.label {
            if &node.label != expected {
                return false;
            }
        }
        self.property_conditions
            .iter()
            .all(|cond| cond.matches_properties(&node.properties))
    }
}

// ── Edge filter ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct EdgeFilter {
    pub label: Option<String>,
    pub weight_condition: Option<WeightCondition>,
    pub property_conditions: Vec<PropertyCondition>,
}

impl EdgeFilter {
    pub fn matches(&self, edge: &crate::core::edge::Edge) -> bool {
        if let Some(ref expected) = self.label {
            if &edge.label != expected {
                return false;
            }
        }
        if let Some(ref wc) = self.weight_condition {
            if !wc.matches(edge.weight) {
                return false;
            }
        }
        self.property_conditions
            .iter()
            .all(|cond| cond.matches_properties(&edge.properties))
    }
}

// ── Weight condition ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct WeightCondition {
    pub op: ComparisonOp,
    pub value: f64,
}

impl WeightCondition {
    pub fn matches(&self, actual: f64) -> bool {
        self.op.compare_f64(actual, self.value)
    }
}

// ── Property condition ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct PropertyCondition {
    pub key: String,
    pub op: ComparisonOp,
    pub value: Value,
}

impl PropertyCondition {
    pub fn matches_properties(&self, props: &std::collections::HashMap<String, Value>) -> bool {
        match props.get(&self.key) {
            None => false,
            Some(actual) => self.op.compare_values(actual, &self.value),
        }
    }
}

// ── Comparison operator ───────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum ComparisonOp {
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
}

impl ComparisonOp {
    pub fn compare_values(&self, lhs: &Value, rhs: &Value) -> bool {
        match (lhs, rhs) {
            (Value::Text(a), Value::Text(b)) => self.compare_ord(a, b),
            (Value::Integer(a), Value::Integer(b)) => self.compare_ord(a, b),
            (Value::Float(a), Value::Float(b)) => self.compare_f64(*a, *b),
            (Value::Integer(a), Value::Float(b)) => self.compare_f64(*a as f64, *b),
            (Value::Float(a), Value::Integer(b)) => self.compare_f64(*a, *b as f64),
            (Value::Boolean(a), Value::Boolean(b)) => self.compare_ord(a, b),
            _ => *self == ComparisonOp::Eq && lhs == rhs,
        }
    }

    pub fn compare_f64(&self, lhs: f64, rhs: f64) -> bool {
        match self {
            ComparisonOp::Eq => (lhs - rhs).abs() < f64::EPSILON,
            ComparisonOp::NotEq => (lhs - rhs).abs() >= f64::EPSILON,
            ComparisonOp::Lt => lhs < rhs,
            ComparisonOp::LtEq => lhs <= rhs,
            ComparisonOp::Gt => lhs > rhs,
            ComparisonOp::GtEq => lhs >= rhs,
        }
    }

    fn compare_ord<T: Ord>(&self, lhs: &T, rhs: &T) -> bool {
        match self {
            ComparisonOp::Eq => lhs == rhs,
            ComparisonOp::NotEq => lhs != rhs,
            ComparisonOp::Lt => lhs < rhs,
            ComparisonOp::LtEq => lhs <= rhs,
            ComparisonOp::Gt => lhs > rhs,
            ComparisonOp::GtEq => lhs >= rhs,
        }
    }
}

impl std::fmt::Display for ComparisonOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ComparisonOp::Eq => write!(f, "="),
            ComparisonOp::NotEq => write!(f, "!="),
            ComparisonOp::Lt => write!(f, "<"),
            ComparisonOp::LtEq => write!(f, "<="),
            ComparisonOp::Gt => write!(f, ">"),
            ComparisonOp::GtEq => write!(f, ">="),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{node::Node, node::NodeId, value::Value};

    fn node_with_props(id: u64, label: &str, props: Vec<(&str, Value)>) -> Node {
        let mut n = Node::new(NodeId(id), label);
        for (k, v) in props { n.properties.insert(k.into(), v); }
        n
    }

    // ── NodeFilter ────────────────────────────────────────────────────────────

    #[test]
    fn node_filter_matches_by_label() {
        let node = node_with_props(0, "City", vec![]);
        let filter = NodeFilter { label: Some("City".into()), property_conditions: vec![] };
        assert!(filter.matches(&node));
    }

    #[test]
    fn node_filter_rejects_wrong_label() {
        let node = node_with_props(0, "Person", vec![]);
        let filter = NodeFilter { label: Some("City".into()), property_conditions: vec![] };
        assert!(!filter.matches(&node));
    }

    #[test]
    fn node_filter_passes_when_no_label_specified() {
        let node = node_with_props(0, "Anything", vec![]);
        let filter = NodeFilter::default();
        assert!(filter.matches(&node));
    }

    #[test]
    fn node_filter_matches_property_condition() {
        let node = node_with_props(0, "City", vec![("pop", Value::Integer(5_000_000))]);
        let filter = NodeFilter {
            label: None,
            property_conditions: vec![PropertyCondition {
                key: "pop".into(),
                op: ComparisonOp::Gt,
                value: Value::Integer(1_000_000),
            }],
        };
        assert!(filter.matches(&node));
    }

    #[test]
    fn node_filter_rejects_unmet_property_condition() {
        let node = node_with_props(0, "City", vec![("pop", Value::Integer(500_000))]);
        let filter = NodeFilter {
            label: None,
            property_conditions: vec![PropertyCondition {
                key: "pop".into(),
                op: ComparisonOp::Gt,
                value: Value::Integer(1_000_000),
            }],
        };
        assert!(!filter.matches(&node));
    }

    // ── ComparisonOp ──────────────────────────────────────────────────────────

    #[test]
    fn comparison_op_integers() {
        assert!(ComparisonOp::Eq.compare_values(&Value::Integer(5), &Value::Integer(5)));
        assert!(ComparisonOp::Lt.compare_values(&Value::Integer(3), &Value::Integer(5)));
        assert!(!ComparisonOp::Gt.compare_values(&Value::Integer(3), &Value::Integer(5)));
    }

    #[test]
    fn comparison_op_strings() {
        let a = Value::Text("apple".into());
        let b = Value::Text("banana".into());
        assert!(ComparisonOp::Lt.compare_values(&a, &b));
        assert!(ComparisonOp::NotEq.compare_values(&a, &b));
    }

    #[test]
    fn comparison_op_display() {
        assert_eq!(ComparisonOp::Eq.to_string(), "=");
        assert_eq!(ComparisonOp::GtEq.to_string(), ">=");
    }
}
