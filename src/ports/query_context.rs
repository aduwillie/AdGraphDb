// DatabaseContext — the port that query executors use to access the database.
//
// Query language implementations never import LayeredGraphDatabase directly.
// They receive a &mut dyn DatabaseContext so they stay decoupled from the
// concrete database type.  This also makes query executors trivially testable
// with a mock context.
//
// The context exposes the operations a query executor could need:
//   • Property data (get_node, get_edge, all_nodes, all_edges)
//   • Structure (node/edge counts, contains_node)
//   • Pre-wired algorithms (traverse_bfs, traverse_dfs, shortest_path_dijkstra)
//
// If you add a new algorithm, expose it here.  Query language parsers can then
// name it by keyword (e.g. "TRAVERSE ASTAR FROM N0") without any other changes.

use crate::core::{
    edge::{Edge, EdgeId},
    error::GraphError,
    node::{Node, NodeId},
};

pub trait DatabaseContext {
    fn get_node(&mut self, id: NodeId) -> Result<Option<Node>, GraphError>;
    fn get_all_nodes(&mut self) -> Result<Vec<Node>, GraphError>;

    /// Load only the nodes that carry a specific label.
    ///
    /// Implementations backed by a LabelIndex return the small candidate set
    /// directly — O(label_count) instead of O(N).  Implementations without an
    /// index fall back to a full scan filtered by label.
    fn get_nodes_by_label(&mut self, label: &str) -> Result<Vec<Node>, GraphError>;

    fn get_edge(&mut self, id: EdgeId) -> Result<Option<Edge>, GraphError>;
    fn get_all_edges(&mut self) -> Result<Vec<Edge>, GraphError>;

    fn node_count(&self) -> usize;
    fn edge_count(&self) -> usize;

    fn traverse_bfs(&self, start: NodeId) -> Vec<NodeId>;
    fn traverse_dfs(&self, start: NodeId) -> Vec<NodeId>;
    fn shortest_path_dijkstra(
        &self,
        start: NodeId,
        goal: NodeId,
    ) -> Option<(Vec<NodeId>, f64)>;
}
