// LruCache — Least-Recently-Used eviction cache.
//
// Implementation strategy: generation counters
//
//   Each entry stores the value and a u64 "generation" stamp.
//   A monotonic counter increments on every get or put.
//   When the store is full, we scan for the entry with the lowest generation
//   and evict it.
//
//   Trade-off vs. a doubly-linked-list LRU:
//     • Simpler to read and reason about (great for education)
//     • O(capacity) eviction instead of O(1)
//     • Fine for typical graph DB working sets (hundreds of hot nodes)
//
// Nodes and edges have independent stores and independent capacity limits.

use std::collections::HashMap;

use crate::core::{
    edge::{Edge, EdgeId},
    node::{Node, NodeId},
};
use crate::ports::cache::CachePort;

// ── Internal entry ────────────────────────────────────────────────────────────

struct Entry<V> {
    value: V,
    last_access_generation: u64,
}

// ── LruCache ──────────────────────────────────────────────────────────────────

pub struct LruCache {
    node_capacity: usize,
    edge_capacity: usize,
    nodes: HashMap<NodeId, Entry<Node>>,
    edges: HashMap<EdgeId, Entry<Edge>>,
    /// Monotonic counter incremented on every access.
    generation: u64,
}

impl LruCache {
    pub fn new(node_capacity: usize, edge_capacity: usize) -> Self {
        assert!(node_capacity > 0, "cache capacity must be at least 1");
        assert!(edge_capacity > 0, "cache capacity must be at least 1");
        Self {
            node_capacity,
            edge_capacity,
            nodes: HashMap::new(),
            edges: HashMap::new(),
            generation: 0,
        }
    }

    fn next_generation(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    /// Evict the node with the smallest last_access_generation.
    fn evict_oldest_node(&mut self) {
        if self.nodes.is_empty() {
            return;
        }
        let oldest_key = self
            .nodes
            .iter()
            .min_by_key(|(_, e)| e.last_access_generation)
            .map(|(k, _)| *k)
            .unwrap();
        self.nodes.remove(&oldest_key);
    }

    fn evict_oldest_edge(&mut self) {
        if self.edges.is_empty() {
            return;
        }
        let oldest_key = self
            .edges
            .iter()
            .min_by_key(|(_, e)| e.last_access_generation)
            .map(|(k, _)| *k)
            .unwrap();
        self.edges.remove(&oldest_key);
    }
}

impl CachePort for LruCache {
    fn get_node(&mut self, id: NodeId) -> Option<Node> {
        let gen = self.next_generation();
        if let Some(entry) = self.nodes.get_mut(&id) {
            entry.last_access_generation = gen;
            Some(entry.value.clone())
        } else {
            None
        }
    }

    fn put_node(&mut self, node: Node) {
        let gen = self.next_generation();

        // If already present, just update.
        if let Some(entry) = self.nodes.get_mut(&node.id) {
            entry.value = node;
            entry.last_access_generation = gen;
            return;
        }

        // Evict before inserting to stay at capacity.
        if self.nodes.len() >= self.node_capacity {
            self.evict_oldest_node();
        }

        self.nodes.insert(node.id, Entry { value: node, last_access_generation: gen });
    }

    fn invalidate_node(&mut self, id: NodeId) {
        self.nodes.remove(&id);
    }

    fn get_edge(&mut self, id: EdgeId) -> Option<Edge> {
        let gen = self.next_generation();
        if let Some(entry) = self.edges.get_mut(&id) {
            entry.last_access_generation = gen;
            Some(entry.value.clone())
        } else {
            None
        }
    }

    fn put_edge(&mut self, edge: Edge) {
        let gen = self.next_generation();

        if let Some(entry) = self.edges.get_mut(&edge.id) {
            entry.value = edge;
            entry.last_access_generation = gen;
            return;
        }

        if self.edges.len() >= self.edge_capacity {
            self.evict_oldest_edge();
        }

        self.edges.insert(edge.id, Entry { value: edge, last_access_generation: gen });
    }

    fn invalidate_edge(&mut self, id: EdgeId) {
        self.edges.remove(&id);
    }

    fn clear(&mut self) {
        self.nodes.clear();
        self.edges.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{node::Node, value::Value};
    use crate::ports::cache::CachePort;

    fn make_node(id: u64, name: &str) -> Node {
        Node::new(NodeId(id), "Person").with_property("name", name)
    }

    #[test]
    fn miss_on_empty_cache() {
        let mut cache = LruCache::new(4, 4);
        assert!(cache.get_node(NodeId(0)).is_none());
    }

    #[test]
    fn put_then_get_returns_node() {
        let mut cache = LruCache::new(4, 4);
        let node = make_node(0, "Alice");
        cache.put_node(node.clone());
        let got = cache.get_node(NodeId(0)).unwrap();
        assert_eq!(got.id, NodeId(0));
    }

    #[test]
    fn invalidate_removes_entry() {
        let mut cache = LruCache::new(4, 4);
        cache.put_node(make_node(0, "Alice"));
        cache.invalidate_node(NodeId(0));
        assert!(cache.get_node(NodeId(0)).is_none());
    }

    #[test]
    fn evicts_lru_when_at_capacity() {
        // Capacity = 2. Insert A, B, then C — A should be evicted (oldest).
        let mut cache = LruCache::new(2, 2);
        cache.put_node(make_node(0, "A")); // gen 1
        cache.put_node(make_node(1, "B")); // gen 2
        cache.put_node(make_node(2, "C")); // triggers eviction of gen-1 (A)
        assert!(cache.get_node(NodeId(0)).is_none()); // A evicted
        assert!(cache.get_node(NodeId(1)).is_some()); // B still here
        assert!(cache.get_node(NodeId(2)).is_some()); // C just inserted
    }

    #[test]
    fn get_updates_recency_preventing_eviction() {
        // Capacity = 2. Insert A, B. Access A. Insert C — B should be evicted.
        let mut cache = LruCache::new(2, 2);
        cache.put_node(make_node(0, "A")); // gen 1
        cache.put_node(make_node(1, "B")); // gen 2
        cache.get_node(NodeId(0));         // A's gen bumped above B's
        cache.put_node(make_node(2, "C")); // evicts B (now the oldest)
        assert!(cache.get_node(NodeId(0)).is_some()); // A still here
        assert!(cache.get_node(NodeId(1)).is_none()); // B evicted
    }

    #[test]
    fn put_overwrites_existing_entry() {
        let mut cache = LruCache::new(4, 4);
        cache.put_node(make_node(0, "Alice"));
        let updated = Node::new(NodeId(0), "Person").with_property("name", "Alicia");
        cache.put_node(updated);
        let got = cache.get_node(NodeId(0)).unwrap();
        assert_eq!(got.properties["name"], Value::Text("Alicia".into()));
    }

    #[test]
    fn clear_empties_all_entries() {
        let mut cache = LruCache::new(4, 4);
        cache.put_node(make_node(0, "A"));
        cache.put_node(make_node(1, "B"));
        cache.clear();
        assert!(cache.get_node(NodeId(0)).is_none());
        assert!(cache.get_node(NodeId(1)).is_none());
    }
}
