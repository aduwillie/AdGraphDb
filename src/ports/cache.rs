// CachePort — fast in-memory read layer that sits in front of StoragePort.
//
// The database checks the cache before touching disk.  On a miss it reads from
// storage and populates the cache so the next access is fast.
//
// get_node / get_edge take &mut self because updating recency counters (LRU)
// is logically a side effect of a read.
//
// Pluggable adapters (see adapters/cache/):
//   • LruCache  — evicts the least-recently-used entry when capacity is full
//   • NoCache   — pass-through, always returns None (useful for testing storage)

use crate::core::{
    edge::{Edge, EdgeId},
    node::{Node, NodeId},
};

pub trait CachePort: Send {
    /// Look up a node. Updates recency if the implementation tracks it (LRU).
    /// Returns a clone so callers own the value and the cache can be mutated
    /// independently.
    fn get_node(&mut self, id: NodeId) -> Option<Node>;

    /// Insert or replace a node in the cache.
    fn put_node(&mut self, node: Node);

    /// Remove a node from the cache (called on delete or explicit invalidation).
    fn invalidate_node(&mut self, id: NodeId);

    /// Look up an edge.
    fn get_edge(&mut self, id: EdgeId) -> Option<Edge>;

    /// Insert or replace an edge in the cache.
    fn put_edge(&mut self, edge: Edge);

    /// Remove an edge from the cache.
    fn invalidate_edge(&mut self, id: EdgeId);

    /// Discard all cached data (e.g. after compaction rewrites storage).
    fn clear(&mut self);
}
