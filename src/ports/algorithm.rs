// Algorithm ports — pluggable graph algorithm interfaces.
//
// Algorithms receive the engine (for structure) and optionally the full
// database (for property access).  They are pure computation: no I/O,
// no mutation of the graph.
//
// To plug in a new algorithm, implement the matching trait and pass it to
// LayeredGraphDatabase::traverse / find_shortest_path.

use crate::core::node::NodeId;
use crate::ports::engine::GraphEnginePort;

// ── Traversal ─────────────────────────────────────────────────────────────────
//
// Visits every reachable node from `start` and returns them in visit order.
// BFS visits level-by-level; DFS follows each branch to exhaustion first.

pub trait TraversalAlgorithm {
    fn traverse(&self, engine: &dyn GraphEnginePort, start: NodeId) -> Vec<NodeId>;
}

// ── Shortest path ─────────────────────────────────────────────────────────────
//
// Finds the minimum-weight path between two nodes.
// Returns the ordered list of node IDs and the total weight, or None if
// no path exists.

pub trait ShortestPathAlgorithm {
    fn find_shortest_path(
        &self,
        engine: &dyn GraphEnginePort,
        start: NodeId,
        goal: NodeId,
    ) -> Option<(Vec<NodeId>, f64)>;
}
