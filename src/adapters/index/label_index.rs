// LabelIndex — in-memory secondary index mapping label → Vec<NodeId>.
//
// Problem it solves:
//   MATCH NODE WHERE label = "City"
//   Currently: loads all N nodes and checks each label → O(N)
//   With this index: one HashMap lookup → O(1), then load only matching nodes
//
// The index is kept strictly in sync with the engine and storage:
//   insert_node  → label_index.insert(id, label)
//   delete_node  → label_index.remove(id, label)
//
// On startup, the index is rebuilt from storage.load_all_nodes(), which
// already returns full Node structs (including label), so no extra I/O.
//
// Trade-offs:
//   + O(1) label lookup instead of O(N) scan
//   + Zero extra disk I/O (built entirely from data already loaded at startup)
//   - One extra HashMap in RAM (tiny — one entry per distinct label)
//   - Each Vec<NodeId> grows with insertions; entries must be removed on delete

use std::collections::HashMap;

use crate::core::node::NodeId;

#[derive(Debug, Default)]
pub struct LabelIndex {
    // "City"   → [N0, N1, N7, N12]
    // "Person" → [N2, N3, N4]
    index: HashMap<String, Vec<NodeId>>,
}

impl LabelIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a node under its label.
    pub fn insert(&mut self, id: NodeId, label: &str) {
        self.index.entry(label.to_string()).or_default().push(id);
    }

    /// Remove a node from its label bucket.
    /// Cleans up the bucket entirely when it becomes empty.
    pub fn remove(&mut self, id: NodeId, label: &str) {
        if let Some(ids) = self.index.get_mut(label) {
            ids.retain(|&stored| stored != id);
            if ids.is_empty() {
                self.index.remove(label);
            }
        }
    }

    /// All node IDs that carry this label.
    /// Returns an empty slice when no nodes with that label exist.
    pub fn get(&self, label: &str) -> &[NodeId] {
        self.index.get(label).map(Vec::as_slice).unwrap_or(&[])
    }

    /// How many nodes carry this label.
    pub fn label_count(&self, label: &str) -> usize {
        self.index.get(label).map(Vec::len).unwrap_or(0)
    }

    /// Every distinct label currently in the graph.
    pub fn all_labels(&self) -> impl Iterator<Item = &str> {
        self.index.keys().map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(id: u64) -> NodeId { NodeId(id) }

    #[test]
    fn lookup_on_empty_index_returns_empty_slice() {
        let idx = LabelIndex::new();
        assert!(idx.get("City").is_empty());
    }

    #[test]
    fn insert_and_lookup() {
        let mut idx = LabelIndex::new();
        idx.insert(n(0), "City");
        idx.insert(n(1), "City");
        idx.insert(n(2), "Person");

        let cities = idx.get("City");
        assert_eq!(cities.len(), 2);
        assert!(cities.contains(&n(0)));
        assert!(cities.contains(&n(1)));

        let people = idx.get("Person");
        assert_eq!(people.len(), 1);
    }

    #[test]
    fn remove_cleans_up_entry() {
        let mut idx = LabelIndex::new();
        idx.insert(n(0), "City");
        idx.insert(n(1), "City");
        idx.remove(n(0), "City");
        let cities = idx.get("City");
        assert_eq!(cities.len(), 1);
        assert!(cities.contains(&n(1)));
    }

    #[test]
    fn remove_last_node_deletes_label_bucket() {
        let mut idx = LabelIndex::new();
        idx.insert(n(0), "Temp");
        idx.remove(n(0), "Temp");
        // The bucket should be gone entirely.
        assert!(idx.get("Temp").is_empty());
        assert_eq!(idx.all_labels().count(), 0);
    }

    #[test]
    fn label_count() {
        let mut idx = LabelIndex::new();
        idx.insert(n(0), "City");
        idx.insert(n(1), "City");
        assert_eq!(idx.label_count("City"), 2);
        assert_eq!(idx.label_count("Missing"), 0);
    }

    #[test]
    fn all_labels() {
        let mut idx = LabelIndex::new();
        idx.insert(n(0), "City");
        idx.insert(n(1), "Person");
        let mut labels: Vec<&str> = idx.all_labels().collect();
        labels.sort();
        assert_eq!(labels, vec!["City", "Person"]);
    }

    #[test]
    fn remove_nonexistent_does_not_panic() {
        let mut idx = LabelIndex::new();
        idx.remove(n(99), "NeverInserted"); // should be a no-op
    }
}
