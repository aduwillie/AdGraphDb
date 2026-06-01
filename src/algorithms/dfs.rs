// DepthFirstSearch — depth-first graph traversal (iterative).
//
// Algorithm (iterative, using an explicit stack):
//   1. Push the start node onto the stack.
//   2. While the stack is non-empty:
//      a. Pop the top node.
//      b. If already visited, skip.
//      c. Visit it (append to result).
//      d. Push all unvisited outgoing neighbors.
//
// Iterative DFS avoids stack-overflow on very deep graphs (unlike recursive).
// Note: neighbor push order affects visit order.  We push in the order the
// engine returns them, so the first neighbor is visited deepest first.
//
// Time complexity:  O(V + E)
// Space complexity: O(V)

use std::collections::HashSet;

use crate::core::node::NodeId;
use crate::ports::{algorithm::TraversalAlgorithm, engine::GraphEnginePort};

pub struct DepthFirstSearch;

impl TraversalAlgorithm for DepthFirstSearch {
    fn traverse(&self, engine: &dyn GraphEnginePort, start: NodeId) -> Vec<NodeId> {
        if !engine.contains_node(start) {
            return vec![];
        }

        let mut visited: HashSet<NodeId> = HashSet::new();
        let mut stack: Vec<NodeId> = vec![start];
        let mut order: Vec<NodeId> = Vec::new();

        while let Some(current) = stack.pop() {
            if !visited.insert(current) {
                // Already visited — the stack can contain duplicates.
                continue;
            }

            order.push(current);

            for neighbor in engine.neighbors_outgoing(current) {
                if !visited.contains(&neighbor.node_id) {
                    stack.push(neighbor.node_id);
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
    use crate::core::edge::EdgeId;
    use crate::ports::{algorithm::TraversalAlgorithm, engine::GraphEnginePort};

    fn n(id: u64) -> NodeId { NodeId(id) }
    fn e(id: u64) -> EdgeId { EdgeId(id) }

    fn linear_graph() -> AdjacencyListEngine {
        // 0 → 1 → 2 → 3
        let mut eng = AdjacencyListEngine::new();
        for i in 0..4 { eng.insert_node(n(i)); }
        eng.insert_edge(e(0), n(0), n(1), 1.0);
        eng.insert_edge(e(1), n(1), n(2), 1.0);
        eng.insert_edge(e(2), n(2), n(3), 1.0);
        eng
    }

    #[test]
    fn start_is_first() {
        let eng = linear_graph();
        let result = DepthFirstSearch.traverse(&eng, n(0));
        assert_eq!(result[0], n(0));
    }

    #[test]
    fn visits_all_nodes_on_linear_graph() {
        let eng = linear_graph();
        let mut result = DepthFirstSearch.traverse(&eng, n(0));
        result.sort_by_key(|id| id.0);
        assert_eq!(result, vec![n(0), n(1), n(2), n(3)]);
    }

    #[test]
    fn no_revisit_on_cycle() {
        let mut eng = AdjacencyListEngine::new();
        eng.insert_node(n(0));
        eng.insert_node(n(1));
        eng.insert_edge(e(0), n(0), n(1), 1.0);
        eng.insert_edge(e(1), n(1), n(0), 1.0);
        let result = DepthFirstSearch.traverse(&eng, n(0));
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn isolated_node_returns_just_itself() {
        let mut eng = AdjacencyListEngine::new();
        eng.insert_node(n(0));
        let result = DepthFirstSearch.traverse(&eng, n(0));
        assert_eq!(result, vec![n(0)]);
    }

    #[test]
    fn missing_start_returns_empty() {
        let eng = linear_graph();
        assert!(DepthFirstSearch.traverse(&eng, n(99)).is_empty());
    }
}
