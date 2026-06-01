// BreadthFirstSearch — level-order graph traversal.
//
// Algorithm:
//   1. Enqueue the start node.
//   2. While the queue is non-empty:
//      a. Dequeue the front node.
//      b. Visit it (append to result).
//      c. Enqueue all unvisited outgoing neighbors.
//
// Uses a VecDeque as the FIFO queue.
// Uses a HashSet to track visited nodes and avoid cycles.
//
// Time complexity:  O(V + E)
// Space complexity: O(V)
//
// BFS guarantees that nodes are returned in non-decreasing hop-distance
// order from the start node.  This makes it ideal for finding the shortest
// *hop* path (ignoring weights).  For weighted shortest paths use Dijkstra.

use std::collections::{HashSet, VecDeque};

use crate::core::node::NodeId;
use crate::ports::{algorithm::TraversalAlgorithm, engine::GraphEnginePort};

pub struct BreadthFirstSearch;

impl TraversalAlgorithm for BreadthFirstSearch {
    fn traverse(&self, engine: &dyn GraphEnginePort, start: NodeId) -> Vec<NodeId> {
        if !engine.contains_node(start) {
            return vec![];
        }

        let mut visited: HashSet<NodeId> = HashSet::new();
        let mut queue: VecDeque<NodeId> = VecDeque::new();
        let mut order: Vec<NodeId> = Vec::new();

        queue.push_back(start);
        visited.insert(start);

        while let Some(current) = queue.pop_front() {
            order.push(current);

            for neighbor in engine.neighbors_outgoing(current) {
                if visited.insert(neighbor.node_id) {
                    // insert returns true when the value was not previously present
                    queue.push_back(neighbor.node_id);
                }
            }
        }

        order
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::engine::adjacency_list::AdjacencyListEngine;
    use crate::ports::{algorithm::TraversalAlgorithm, engine::GraphEnginePort};

    fn n(id: u64) -> NodeId { NodeId(id) }

    /// Build:  0 → 1 → 3
    ///         0 → 2 → 3
    fn diamond_graph() -> AdjacencyListEngine {
        let mut eng = AdjacencyListEngine::new();
        for i in 0..4 { eng.insert_node(n(i)); }
        eng.insert_edge(crate::core::edge::EdgeId(0), n(0), n(1), 1.0);
        eng.insert_edge(crate::core::edge::EdgeId(1), n(0), n(2), 1.0);
        eng.insert_edge(crate::core::edge::EdgeId(2), n(1), n(3), 1.0);
        eng.insert_edge(crate::core::edge::EdgeId(3), n(2), n(3), 1.0);
        eng
    }

    #[test]
    fn start_node_is_first() {
        let eng = diamond_graph();
        let result = BreadthFirstSearch.traverse(&eng, n(0));
        assert_eq!(result[0], n(0));
    }

    #[test]
    fn visits_all_reachable_nodes() {
        let eng = diamond_graph();
        let mut result = BreadthFirstSearch.traverse(&eng, n(0));
        result.sort_by_key(|id| id.0);
        assert_eq!(result, vec![n(0), n(1), n(2), n(3)]);
    }

    #[test]
    fn level_order_respected() {
        // 0 is level 0, {1,2} are level 1, {3} is level 2.
        let eng = diamond_graph();
        let result = BreadthFirstSearch.traverse(&eng, n(0));
        // 0 must come before 1 and 2; 1 and 2 must come before 3.
        let pos: std::collections::HashMap<NodeId, usize> =
            result.iter().enumerate().map(|(i, &id)| (id, i)).collect();
        assert!(pos[&n(0)] < pos[&n(1)]);
        assert!(pos[&n(0)] < pos[&n(2)]);
        assert!(pos[&n(1)] < pos[&n(3)]);
        assert!(pos[&n(2)] < pos[&n(3)]);
    }

    #[test]
    fn no_revisit_on_cycle() {
        let mut eng = AdjacencyListEngine::new();
        eng.insert_node(n(0));
        eng.insert_node(n(1));
        eng.insert_edge(crate::core::edge::EdgeId(0), n(0), n(1), 1.0);
        eng.insert_edge(crate::core::edge::EdgeId(1), n(1), n(0), 1.0); // cycle
        let result = BreadthFirstSearch.traverse(&eng, n(0));
        assert_eq!(result.len(), 2); // visited exactly twice, no infinite loop
    }

    #[test]
    fn unreachable_node_not_visited() {
        let mut eng = diamond_graph();
        eng.insert_node(n(99)); // disconnected
        let result = BreadthFirstSearch.traverse(&eng, n(0));
        assert!(!result.contains(&n(99)));
    }

    #[test]
    fn missing_start_node_returns_empty() {
        let eng = diamond_graph();
        assert!(BreadthFirstSearch.traverse(&eng, n(999)).is_empty());
    }
}
