// PropertyIndex — per-field sorted secondary indexes for fast property-filter queries.
//
// Problem it solves:
//   MATCH NODE WHERE props.population > 1_000_000
//   Currently (without this index): O(N) — loads every node and checks the field.
//   With this index:                O(log N + results) — binary-search on a BTreeMap.
//
// Design
// ──────
// Each indexed field gets its own typed sub-indexes (one BTreeMap per value type).
// This sidesteps the `f64: !Ord` limitation cleanly — floats go into the float
// sub-index, integers into the integer sub-index, etc.
//
//   field "population" →
//       integers: BTreeMap { 17_000 → [N2], 1_200_000 → [N3], 9_000_000 → [N0] }
//
//   field "name" →
//       texts: BTreeMap { "Berlin" → [N1], "London" → [N0], "Paris" → [N2] }
//
// The index is rebuilt entirely from storage on startup — no separate on-disk file.
// It is kept in sync with the engine: insert/delete update both.
//
// Trade-offs
// ──────────
//   + O(log N + k) equality and range queries (k = result count)
//   + Zero extra disk I/O at startup (derived from data already loaded)
//   - Extra RAM: one BTreeMap entry per (field, value, nodeId) triple
//   - Insert/delete are O(log N) instead of O(1)

use std::collections::{BTreeMap, HashMap};

use crate::core::{node::NodeId, value::Value};
use crate::query::ast::ComparisonOp;

// ── Ordered float wrapper ─────────────────────────────────────────────────────
//
// f64 does not implement Ord because NaN != NaN.  We provide a total order that
// treats NaN as the smallest possible value so it sorts below all real numbers.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrderedF64(pub f64);

impl Eq for OrderedF64 {}

impl PartialOrd for OrderedF64 {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderedF64 {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // NaN sorted below -∞
        self.0.partial_cmp(&other.0).unwrap_or_else(|| {
            match (self.0.is_nan(), other.0.is_nan()) {
                (true, true)  => std::cmp::Ordering::Equal,
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _             => std::cmp::Ordering::Equal,
            }
        })
    }
}

// ── Per-field sub-indexes ─────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct FieldIndex {
    integers: BTreeMap<i64,      Vec<NodeId>>,
    floats:   BTreeMap<OrderedF64, Vec<NodeId>>,
    texts:    BTreeMap<String,   Vec<NodeId>>,
    bools:    BTreeMap<bool,     Vec<NodeId>>,
    nulls:    Vec<NodeId>,
}

impl FieldIndex {
    fn insert(&mut self, id: NodeId, value: &Value) {
        match value {
            Value::Integer(v) => bucket_insert(&mut self.integers, *v, id),
            Value::Float(v)   => bucket_insert(&mut self.floats, OrderedF64(*v), id),
            Value::Text(v)    => bucket_insert(&mut self.texts, v.clone(), id),
            Value::Boolean(v) => bucket_insert(&mut self.bools, *v, id),
            Value::Null       => self.nulls.push(id),
        }
    }

    fn remove(&mut self, id: NodeId, value: &Value) {
        match value {
            Value::Integer(v) => bucket_remove(&mut self.integers, v, id),
            Value::Float(v)   => bucket_remove(&mut self.floats, &OrderedF64(*v), id),
            Value::Text(v)    => bucket_remove(&mut self.texts, v, id),
            Value::Boolean(v) => bucket_remove(&mut self.bools, v, id),
            Value::Null       => self.nulls.retain(|&n| n != id),
        }
    }

    /// Return node IDs satisfying `field <op> value`.
    fn query(&self, op: &ComparisonOp, value: &Value) -> Vec<NodeId> {
        match value {
            Value::Integer(v) => range_query_btree(&self.integers, op, v),
            Value::Float(v)   => range_query_btree(&self.floats, op, &OrderedF64(*v)),
            Value::Text(v)    => range_query_btree(&self.texts, op, v),
            Value::Boolean(v) => range_query_btree(&self.bools, op, v),
            Value::Null => {
                if *op == ComparisonOp::Eq {
                    self.nulls.clone()
                } else {
                    vec![]
                }
            }
        }
    }
}

// ── PropertyIndex — the public type ──────────────────────────────────────────

#[derive(Debug, Default)]
pub struct PropertyIndex {
    /// field_name → typed sub-index
    fields: HashMap<String, FieldIndex>,
}

impl PropertyIndex {
    pub fn new() -> Self { Self::default() }

    // ── Write operations ──────────────────────────────────────────────────────

    /// Index all properties of a newly-inserted node.
    pub fn insert_node(&mut self, id: NodeId, props: &std::collections::HashMap<String, Value>) {
        for (field, value) in props {
            self.fields
                .entry(field.clone())
                .or_default()
                .insert(id, value);
        }
    }

    /// Remove all indexed properties of a deleted node.
    pub fn remove_node(&mut self, id: NodeId, props: &std::collections::HashMap<String, Value>) {
        for (field, value) in props {
            if let Some(fi) = self.fields.get_mut(field) {
                fi.remove(id, value);
            }
        }
    }

    // ── Query operations ──────────────────────────────────────────────────────

    /// Return node IDs where `field <op> value`, or None if no index exists
    /// for that field.
    ///
    /// Returning None signals the caller to fall back to a full scan.
    /// Returning Some([]) means the index was consulted and found nothing.
    pub fn query(
        &self,
        field: &str,
        op: &ComparisonOp,
        value: &Value,
    ) -> Option<Vec<NodeId>> {
        self.fields.get(field).map(|fi| fi.query(op, value))
    }

    /// True if there is an index for this property field.
    pub fn is_indexed(&self, field: &str) -> bool {
        self.fields.contains_key(field)
    }

    /// All currently-indexed field names.
    pub fn indexed_fields(&self) -> impl Iterator<Item = &str> {
        self.fields.keys().map(String::as_str)
    }

    /// Approximate number of entries across all sub-indexes for a field.
    pub fn field_cardinality(&self, field: &str) -> usize {
        match self.fields.get(field) {
            None => 0,
            Some(fi) => {
                fi.integers.len()
                    + fi.floats.len()
                    + fi.texts.len()
                    + fi.bools.len()
                    + if fi.nulls.is_empty() { 0 } else { 1 }
            }
        }
    }
}

// ── BTree range query helper ──────────────────────────────────────────────────

fn range_query_btree<K: Ord + Clone>(
    map: &BTreeMap<K, Vec<NodeId>>,
    op: &ComparisonOp,
    key: &K,
) -> Vec<NodeId> {
    use std::ops::Bound::*;

    let iter: Box<dyn Iterator<Item = &Vec<NodeId>>> = match op {
        ComparisonOp::Eq   => {
            return map.get(key).cloned().unwrap_or_default();
        }
        ComparisonOp::NotEq => {
            // Everything except the matching bucket
            return map
                .iter()
                .filter(|(k, _)| *k != key)
                .flat_map(|(_, ids)| ids.iter().copied())
                .collect();
        }
        ComparisonOp::Lt   => Box::new(map.range(..key.clone()).map(|(_, v)| v)),
        ComparisonOp::LtEq => Box::new(map.range(..=key.clone()).map(|(_, v)| v)),
        ComparisonOp::Gt   => {
            let cloned = key.clone();
            // Exclusive lower bound: skip the key itself
            Box::new(map.range((Excluded(cloned), Unbounded)).map(|(_, v)| v))
        }
        ComparisonOp::GtEq => Box::new(map.range(key.clone()..).map(|(_, v)| v)),
    };

    iter.flat_map(|ids| ids.iter().copied()).collect()
}

fn bucket_insert<K: Ord>(map: &mut BTreeMap<K, Vec<NodeId>>, key: K, id: NodeId) {
    map.entry(key).or_default().push(id);
}

fn bucket_remove<K: Ord>(map: &mut BTreeMap<K, Vec<NodeId>>, key: &K, id: NodeId) {
    if let Some(ids) = map.get_mut(key) {
        ids.retain(|&n| n != id);
        if ids.is_empty() {
            map.remove(key);
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn n(id: u64) -> NodeId { NodeId(id) }

    fn city_props(name: &str, pop: i64) -> HashMap<String, Value> {
        let mut p = HashMap::new();
        p.insert("name".into(), Value::Text(name.into()));
        p.insert("population".into(), Value::Integer(pop));
        p
    }

    fn build_index() -> PropertyIndex {
        let mut idx = PropertyIndex::new();
        idx.insert_node(n(0), &city_props("London",   9_000_000));
        idx.insert_node(n(1), &city_props("Paris",    2_100_000));
        idx.insert_node(n(2), &city_props("Berlin",   3_700_000));
        idx.insert_node(n(3), &city_props("Brussels", 1_200_000));
        idx
    }

    #[test]
    fn equality_integer() {
        let idx = build_index();
        let ids = idx.query("population", &ComparisonOp::Eq, &Value::Integer(2_100_000)).unwrap();
        assert_eq!(ids, vec![n(1)]);
    }

    #[test]
    fn greater_than_integer() {
        let idx = build_index();
        let mut ids = idx.query("population", &ComparisonOp::Gt, &Value::Integer(2_000_000)).unwrap();
        ids.sort_by_key(|n| n.0);
        // London 9M, Paris 2.1M, Berlin 3.7M all > 2_000_000
        assert_eq!(ids, vec![n(0), n(1), n(2)]);
    }

    #[test]
    fn less_than_equal_integer() {
        let idx = build_index();
        let mut ids = idx.query("population", &ComparisonOp::LtEq, &Value::Integer(2_100_000)).unwrap();
        ids.sort_by_key(|n| n.0);
        assert_eq!(ids, vec![n(1), n(3)]);   // Paris 2.1M, Brussels 1.2M
    }

    #[test]
    fn range_greater_than_equal() {
        let idx = build_index();
        let mut ids = idx.query("population", &ComparisonOp::GtEq, &Value::Integer(3_700_000)).unwrap();
        ids.sort_by_key(|n| n.0);
        assert_eq!(ids, vec![n(0), n(2)]);   // London 9M, Berlin 3.7M
    }

    #[test]
    fn text_equality() {
        let idx = build_index();
        let ids = idx.query("name", &ComparisonOp::Eq, &Value::Text("Berlin".into())).unwrap();
        assert_eq!(ids, vec![n(2)]);
    }

    #[test]
    fn missing_field_returns_none() {
        let idx = build_index();
        assert!(idx.query("altitude", &ComparisonOp::Eq, &Value::Integer(0)).is_none());
    }

    #[test]
    fn remove_node_clears_entries() {
        let mut idx = build_index();
        idx.remove_node(n(0), &city_props("London", 9_000_000));
        let ids = idx.query("population", &ComparisonOp::Gt, &Value::Integer(8_000_000)).unwrap();
        assert!(ids.is_empty());
    }

    #[test]
    fn is_indexed_after_insert() {
        let idx = build_index();
        assert!(idx.is_indexed("population"));
        assert!(!idx.is_indexed("altitude"));
    }

    #[test]
    fn boolean_equality() {
        let mut idx = PropertyIndex::new();
        let mut p = HashMap::new();
        p.insert("active".into(), Value::Boolean(true));
        idx.insert_node(n(0), &p);
        p.insert("active".into(), Value::Boolean(false));
        idx.insert_node(n(1), &p);

        let t = idx.query("active", &ComparisonOp::Eq, &Value::Boolean(true)).unwrap();
        assert_eq!(t, vec![n(0)]);
    }

    #[test]
    fn float_range() {
        let mut idx = PropertyIndex::new();
        let mut p = HashMap::new();
        p.insert("weight".into(), Value::Float(1.5));
        idx.insert_node(n(0), &p);
        p.insert("weight".into(), Value::Float(3.0));
        idx.insert_node(n(1), &p);

        let ids = idx.query("weight", &ComparisonOp::Gt, &Value::Float(2.0)).unwrap();
        assert_eq!(ids, vec![n(1)]);
    }
}
