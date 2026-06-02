// DatabaseContext — the port that query executors use to access the database.
//
// Query language implementations never import LayeredGraphDatabase directly.
// They receive a &mut dyn DatabaseContext so they stay decoupled from the
// concrete database type.  This also makes query executors trivially testable
// with a mock context.
//
// The context provides:
//   • Property data       — get_node, get_edge, all_nodes, all_edges
//   • Index fast-paths    — get_nodes_by_label, get_nodes_by_property
//   • Structure           — node/edge counts
//   • Pre-wired algorithms — traverse_bfs, traverse_dfs, shortest_path_dijkstra
//   • Planner input       — stats() returns DatabaseStats for cost estimation

use crate::core::{
    edge::{Edge, EdgeId},
    error::GraphError,
    node::{Node, NodeId},
    value::Value,
};
use crate::query::{ast::ComparisonOp, planner::DatabaseStats};

pub trait DatabaseContext {
    // ── Property data ─────────────────────────────────────────────────────────

    fn get_node(&mut self, id: NodeId) -> Result<Option<Node>, GraphError>;
    fn get_all_nodes(&mut self) -> Result<Vec<Node>, GraphError>;

    /// Fast-path: returns only nodes that carry `label` using the LabelIndex —
    /// O(label_count) instead of O(N).
    fn get_nodes_by_label(&mut self, label: &str) -> Result<Vec<Node>, GraphError>;

    /// Fast-path: returns only nodes where `field <op> value` using the
    /// PropertyIndex — O(log N + results) instead of O(N).
    fn get_nodes_by_property(
        &mut self,
        field: &str,
        op: &ComparisonOp,
        value: &Value,
    ) -> Result<Vec<Node>, GraphError>;

    fn get_edge(&mut self, id: EdgeId) -> Result<Option<Edge>, GraphError>;
    fn get_all_edges(&mut self) -> Result<Vec<Edge>, GraphError>;

    // ── Counts ────────────────────────────────────────────────────────────────

    fn node_count(&self) -> usize;
    fn edge_count(&self) -> usize;

    // ── Algorithms ────────────────────────────────────────────────────────────

    fn traverse_bfs(&self, start: NodeId) -> Vec<NodeId>;
    fn traverse_dfs(&self, start: NodeId) -> Vec<NodeId>;
    fn shortest_path_dijkstra(
        &self,
        start: NodeId,
        goal:  NodeId,
    ) -> Option<(Vec<NodeId>, f64)>;

    // ── Planner input ─────────────────────────────────────────────────────────

    /// Snapshot of database statistics for the QueryPlanner's cost model.
    /// Cheap — derived from in-memory indexes (no I/O).
    fn stats(&self) -> DatabaseStats;
}
