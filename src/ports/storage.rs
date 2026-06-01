// StoragePort — durability boundary.
//
// Implementations must ensure data survives a process restart.
// The database calls this port on every write and on cache misses.
//
// Pluggable adapters (see adapters/storage/):
//   • JsonFileStorage  — newline-delimited JSON WAL (human readable)
//   • BinaryFileStorage — custom binary WAL (educational hand-written codec)

use crate::core::{
    edge::{Edge, EdgeId},
    error::GraphError,
    node::{Node, NodeId},
};

pub trait StoragePort {
    /// Persist a node, overwriting any previous version with the same id.
    fn save_node(&mut self, node: &Node) -> Result<(), GraphError>;

    /// Retrieve a single node by id. Returns None if it does not exist.
    fn load_node(&self, id: NodeId) -> Result<Option<Node>, GraphError>;

    /// Mark a node as deleted. Subsequent loads must return None.
    fn delete_node(&mut self, id: NodeId) -> Result<(), GraphError>;

    /// Persist an edge, overwriting any previous version with the same id.
    fn save_edge(&mut self, edge: &Edge) -> Result<(), GraphError>;

    /// Retrieve a single edge by id. Returns None if it does not exist.
    fn load_edge(&self, id: EdgeId) -> Result<Option<Edge>, GraphError>;

    /// Mark an edge as deleted.
    fn delete_edge(&mut self, id: EdgeId) -> Result<(), GraphError>;

    /// Scan all live nodes (used on startup to rebuild the engine and seed IDs).
    fn load_all_nodes(&self) -> Result<Vec<Node>, GraphError>;

    /// Scan all live edges (used on startup to rebuild the engine).
    fn load_all_edges(&self) -> Result<Vec<Edge>, GraphError>;

    /// Rewrite the backing store containing only the current live state.
    /// WAL-style adapters accumulate tombstones and overwrites; compaction
    /// discards them, keeping file size proportional to live data.
    fn compact(&mut self) -> Result<(), GraphError>;
}
