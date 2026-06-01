// AdjacencyListEngine — GraphEnginePort backed by two HashMaps.
//
// Data structure overview:
//
//   outgoing: HashMap<NodeId, Vec<Neighbor>>
//     For each node: list of (target, edge_id, weight) triples.
//
//   incoming: HashMap<NodeId, Vec<Neighbor>>
//     For each node: list of (source, edge_id, weight) triples.
//     Maintained as a mirror so incoming queries are also O(degree).
//
//   edge_endpoints: HashMap<EdgeId, (NodeId, NodeId)>
//     Lets remove_edge find the source/target without a full scan.
//
// This is the canonical sparse-graph representation.
// A dense graph with many edges between the same nodes would benefit from
// an adjacency-matrix engine instead (swap the adapter; the rest is unchanged).

use std::collections::HashMap;

use crate::core::{edge::EdgeId, node::NodeId};
use crate::ports::engine::{GraphEnginePort, Neighbor};

#[derive(Debug, Default)]
pub struct AdjacencyListEngine {
    outgoing: HashMap<NodeId, Vec<Neighbor>>,
    incoming: HashMap<NodeId, Vec<Neighbor>>,
    /// Stores (source, target) for each edge so we can clean both lists on removal.
    edge_endpoints: HashMap<EdgeId, (NodeId, NodeId)>,
}

impl AdjacencyListEngine {
    pub fn new() -> Self {
        Self::default()
    }
}

impl GraphEnginePort for AdjacencyListEngine {
    fn insert_node(&mut self, id: NodeId) {
        self.outgoing.entry(id).or_default();
        self.incoming.entry(id).or_default();
    }

    fn remove_node(&mut self, id: NodeId) {
        // Collect edge ids that touch this node, then remove each.
        let edge_ids_to_remove: Vec<EdgeId> = self
            .edge_endpoints
            .iter()
            .filter(|(_, (src, tgt))| *src == id || *tgt == id)
            .map(|(eid, _)| *eid)
            .collect();

        for eid in edge_ids_to_remove {
            self.remove_edge(eid);
        }

        self.outgoing.remove(&id);
        self.incoming.remove(&id);
    }

    fn insert_edge(&mut self, edge_id: EdgeId, source: NodeId, target: NodeId, weight: f64) {
        self.edge_endpoints.insert(edge_id, (source, target));

        self.outgoing.entry(source).or_default().push(Neighbor {
            node_id: target,
            edge_id,
            weight,
        });

        self.incoming.entry(target).or_default().push(Neighbor {
            node_id: source,
            edge_id,
            weight,
        });
    }

    fn remove_edge(&mut self, edge_id: EdgeId) {
        let Some((source, target)) = self.edge_endpoints.remove(&edge_id) else {
            return;
        };

        if let Some(neighbors) = self.outgoing.get_mut(&source) {
            neighbors.retain(|n| n.edge_id != edge_id);
        }

        if let Some(neighbors) = self.incoming.get_mut(&target) {
            neighbors.retain(|n| n.edge_id != edge_id);
        }
    }

    fn neighbors_outgoing(&self, source: NodeId) -> Vec<Neighbor> {
        self.outgoing.get(&source).cloned().unwrap_or_default()
    }

    fn neighbors_incoming(&self, target: NodeId) -> Vec<Neighbor> {
        self.incoming.get(&target).cloned().unwrap_or_default()
    }

    fn all_node_ids(&self) -> Vec<NodeId> {
        self.outgoing.keys().copied().collect()
    }

    fn all_edge_ids(&self) -> Vec<EdgeId> {
        self.edge_endpoints.keys().copied().collect()
    }

    fn node_count(&self) -> usize {
        self.outgoing.len()
    }

    fn edge_count(&self) -> usize {
        self.edge_endpoints.len()
    }

    fn contains_node(&self, id: NodeId) -> bool {
        self.outgoing.contains_key(&id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::engine::GraphEnginePort;

    fn n(id: u64) -> NodeId { NodeId(id) }
    fn e(id: u64) -> EdgeId { EdgeId(id) }

    fn three_node_graph() -> AdjacencyListEngine {
        // A(0) --e0--> B(1) --e1--> C(2)
        //              B(1) --e2--> A(0)
        let mut eng = AdjacencyListEngine::new();
        eng.insert_node(n(0));
        eng.insert_node(n(1));
        eng.insert_node(n(2));
        eng.insert_edge(e(0), n(0), n(1), 1.0);
        eng.insert_edge(e(1), n(1), n(2), 2.0);
        eng.insert_edge(e(2), n(1), n(0), 3.0);
        eng
    }

    #[test]
    fn node_count_after_inserts() {
        let eng = three_node_graph();
        assert_eq!(eng.node_count(), 3);
    }

    #[test]
    fn edge_count_after_inserts() {
        let eng = three_node_graph();
        assert_eq!(eng.edge_count(), 3);
    }

    #[test]
    fn contains_node_true_for_inserted() {
        let eng = three_node_graph();
        assert!(eng.contains_node(n(0)));
        assert!(eng.contains_node(n(2)));
    }

    #[test]
    fn contains_node_false_for_missing() {
        let eng = three_node_graph();
        assert!(!eng.contains_node(n(99)));
    }

    #[test]
    fn outgoing_neighbors_correct() {
        let eng = three_node_graph();
        let neighbors = eng.neighbors_outgoing(n(1));
        let targets: Vec<NodeId> = neighbors.iter().map(|nb| nb.node_id).collect();
        assert!(targets.contains(&n(2)));
        assert!(targets.contains(&n(0)));
    }

    #[test]
    fn incoming_neighbors_correct() {
        let eng = three_node_graph();
        let neighbors = eng.neighbors_incoming(n(1));
        let sources: Vec<NodeId> = neighbors.iter().map(|nb| nb.node_id).collect();
        assert!(sources.contains(&n(0)));
    }

    #[test]
    fn remove_edge_cleans_both_lists() {
        let mut eng = three_node_graph();
        eng.remove_edge(e(0)); // A→B
        assert!(eng.neighbors_outgoing(n(0)).is_empty());
        let incoming_b: Vec<_> = eng.neighbors_incoming(n(1)).iter()
            .map(|nb| nb.edge_id)
            .collect();
        assert!(!incoming_b.contains(&e(0)));
    }

    #[test]
    fn remove_node_also_removes_incident_edges() {
        let mut eng = three_node_graph();
        eng.remove_node(n(1)); // B has 3 incident edges
        assert_eq!(eng.node_count(), 2);
        assert_eq!(eng.edge_count(), 0); // all edges touched B
    }

    #[test]
    fn all_node_ids_returns_all() {
        let eng = three_node_graph();
        let mut ids = eng.all_node_ids();
        ids.sort_by_key(|id| id.0);
        assert_eq!(ids, vec![n(0), n(1), n(2)]);
    }

    #[test]
    fn weight_stored_on_neighbor() {
        let eng = three_node_graph();
        let neighbor = eng.neighbors_outgoing(n(0)).into_iter().next().unwrap();
        assert!((neighbor.weight - 1.0).abs() < f64::EPSILON);
    }
}
