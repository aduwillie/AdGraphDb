// Dijkstra's shortest-path algorithm.
//
// Finds the minimum-weight path from `start` to `goal` in a graph with
// non-negative edge weights.
//
// Algorithm:
//   1. Assign distance[start] = 0, distance[all others] = ∞.
//   2. Maintain a priority queue (min-heap) of (distance, node_id).
//   3. While the queue is non-empty:
//      a. Pop the node with the smallest tentative distance.
//      b. If it equals goal, reconstruct and return the path.
//      c. For each outgoing neighbor: if current_dist + weight < known_dist,
//         update known_dist, record the predecessor, and enqueue.
//
// We use std::collections::BinaryHeap with reversed ordering (via Reverse)
// to turn Rust's max-heap into a min-heap.
//
// Time complexity:  O((V + E) log V)   with a binary heap
// Space complexity: O(V)
//
// Precondition: all edge weights must be ≥ 0.
// Negative weights require Bellman-Ford instead.

use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashMap},
};

use crate::core::node::NodeId;
use crate::ports::{algorithm::ShortestPathAlgorithm, engine::GraphEnginePort};

pub struct Dijkstra;

/// Ordered by distance for use in a BinaryHeap (wrapped in Reverse for min-heap behaviour).
#[derive(PartialEq)]
struct HeapEntry {
    distance: f64,
    node_id: NodeId,
}

impl Eq for HeapEntry {}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // total_cmp handles NaN consistently; f64 otherwise lacks Ord.
        self.distance
            .total_cmp(&other.distance)
            .then_with(|| self.node_id.0.cmp(&other.node_id.0))
    }
}

impl ShortestPathAlgorithm for Dijkstra {
    fn find_shortest_path(
        &self,
        engine: &dyn GraphEnginePort,
        start: NodeId,
        goal: NodeId,
    ) -> Option<(Vec<NodeId>, f64)> {
        if !engine.contains_node(start) || !engine.contains_node(goal) {
            return None;
        }

        // distance[node] = best known distance from start to node
        let mut distance: HashMap<NodeId, f64> = HashMap::new();
        // predecessor[node] = the node we arrived from on the best path
        let mut predecessor: HashMap<NodeId, NodeId> = HashMap::new();

        distance.insert(start, 0.0);

        // Rust's BinaryHeap is a max-heap; Reverse turns it into a min-heap.
        let mut heap: BinaryHeap<Reverse<HeapEntry>> = BinaryHeap::new();
        heap.push(Reverse(HeapEntry { distance: 0.0, node_id: start }));

        while let Some(Reverse(HeapEntry { distance: current_dist, node_id: current })) =
            heap.pop()
        {
            if current == goal {
                // Reconstruct the path by walking predecessors back to start.
                let total_weight = *distance.get(&goal).unwrap();
                let path = reconstruct_path(&predecessor, start, goal);
                return Some((path, total_weight));
            }

            // Skip stale heap entries (we may have inserted a node multiple times
            // with different distances; only the smallest matters).
            if current_dist > *distance.get(&current).unwrap_or(&f64::INFINITY) {
                continue;
            }

            for neighbor in engine.neighbors_outgoing(current) {
                let new_dist = current_dist + neighbor.weight;
                let known_dist = *distance.get(&neighbor.node_id).unwrap_or(&f64::INFINITY);

                if new_dist < known_dist {
                    distance.insert(neighbor.node_id, new_dist);
                    predecessor.insert(neighbor.node_id, current);
                    heap.push(Reverse(HeapEntry {
                        distance: new_dist,
                        node_id: neighbor.node_id,
                    }));
                }
            }
        }

        None // goal is not reachable from start
    }
}

fn reconstruct_path(
    predecessor: &HashMap<NodeId, NodeId>,
    start: NodeId,
    goal: NodeId,
) -> Vec<NodeId> {
    let mut path = Vec::new();
    let mut current = goal;

    loop {
        path.push(current);
        if current == start {
            break;
        }
        match predecessor.get(&current) {
            Some(&prev) => current = prev,
            None => break, // should not happen in a consistent graph
        }
    }

    path.reverse();
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::engine::adjacency_list::AdjacencyListEngine;
    use crate::core::edge::EdgeId;
    use crate::ports::{algorithm::ShortestPathAlgorithm, engine::GraphEnginePort};

    fn n(id: u64) -> NodeId { NodeId(id) }
    fn e(id: u64) -> EdgeId { EdgeId(id) }

    /// Graph:  0 --1.0--> 1 --2.0--> 3
    ///         0 --10.0-> 2 --1.0--> 3
    /// Shortest 0→3: via 1 (cost 3.0), not via 2 (cost 11.0).
    fn weighted_graph() -> AdjacencyListEngine {
        let mut eng = AdjacencyListEngine::new();
        for i in 0..4 { eng.insert_node(n(i)); }
        eng.insert_edge(e(0), n(0), n(1), 1.0);
        eng.insert_edge(e(1), n(1), n(3), 2.0);
        eng.insert_edge(e(2), n(0), n(2), 10.0);
        eng.insert_edge(e(3), n(2), n(3), 1.0);
        eng
    }

    #[test]
    fn finds_shortest_path() {
        let eng = weighted_graph();
        let (path, cost) = Dijkstra.find_shortest_path(&eng, n(0), n(3)).unwrap();
        assert!((cost - 3.0).abs() < f64::EPSILON);
        assert_eq!(path, vec![n(0), n(1), n(3)]);
    }

    #[test]
    fn returns_none_when_no_path() {
        let mut eng = AdjacencyListEngine::new();
        eng.insert_node(n(0));
        eng.insert_node(n(1)); // disconnected
        assert!(Dijkstra.find_shortest_path(&eng, n(0), n(1)).is_none());
    }

    #[test]
    fn path_to_self_has_zero_cost() {
        let mut eng = AdjacencyListEngine::new();
        eng.insert_node(n(0));
        let (path, cost) = Dijkstra.find_shortest_path(&eng, n(0), n(0)).unwrap();
        assert!((cost - 0.0).abs() < f64::EPSILON);
        assert_eq!(path, vec![n(0)]);
    }

    #[test]
    fn missing_start_returns_none() {
        let eng = weighted_graph();
        assert!(Dijkstra.find_shortest_path(&eng, n(99), n(0)).is_none());
    }

    #[test]
    fn missing_goal_returns_none() {
        let eng = weighted_graph();
        assert!(Dijkstra.find_shortest_path(&eng, n(0), n(99)).is_none());
    }

    #[test]
    fn avoids_longer_path() {
        let eng = weighted_graph();
        // The longer path 0→2→3 costs 11.0; we must NOT choose it.
        let (_, cost) = Dijkstra.find_shortest_path(&eng, n(0), n(3)).unwrap();
        assert!(cost < 5.0);
    }
}
