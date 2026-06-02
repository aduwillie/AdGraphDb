// GraphEnginePort — in-memory structural index for the graph.
//
// The engine tracks which nodes exist and which edges connect them.
// It does NOT store property data — that lives in the cache or on disk.
// Its sole job is answering structural questions fast:
//   "Who are the outgoing neighbors of node X?"
//   "What edges does node X have?"
//
// This separation lets us swap graph representations (adjacency list vs matrix)
// without touching persistence or property storage.
//
// Pluggable adapters (see adapters/engine/):
//   • AdjacencyListEngine — HashMap-based, O(1) neighbor lookup, sparse-friendly

use crate::core::{
    edge::EdgeId,
    node::NodeId,
};

/// A lightweight descriptor of one side of an edge — returned by neighbor queries.
#[derive(Debug, Clone, PartialEq)]
pub struct Neighbor {
    pub node_id: NodeId,
    pub edge_id: EdgeId,
    pub weight: f64,
}

pub trait GraphEnginePort: Send {
    /// Register a node's existence in the structural index.
    fn insert_node(&mut self, id: NodeId);

    /// Remove a node and all its incident edges from the index.
    fn remove_node(&mut self, id: NodeId);

    /// Record a directed edge between two nodes.
    fn insert_edge(&mut self, edge_id: EdgeId, source: NodeId, target: NodeId, weight: f64);

    /// Remove an edge from the index.
    fn remove_edge(&mut self, edge_id: EdgeId);

    /// All nodes that source points to, with edge metadata.
    fn neighbors_outgoing(&self, source: NodeId) -> Vec<Neighbor>;

    /// All nodes that point to target, with edge metadata.
    fn neighbors_incoming(&self, target: NodeId) -> Vec<Neighbor>;

    /// Every node id currently in the graph.
    fn all_node_ids(&self) -> Vec<NodeId>;

    /// Every edge id currently in the graph.
    fn all_edge_ids(&self) -> Vec<EdgeId>;

    fn node_count(&self) -> usize;
    fn edge_count(&self) -> usize;
    fn contains_node(&self, id: NodeId) -> bool;
}
