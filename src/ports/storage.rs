// StoragePort — durability boundary.
//
// Implementations must ensure data survives a process restart.
// The database calls this port on every write and on cache misses.
//
// Pluggable adapters (see adapters/storage/):
//   • JsonFileStorage    — newline-delimited JSON WAL (human readable)
//   • BinaryFileStorage  — custom binary WAL with Adler-32 checksums and
//                          crash-safe transaction markers
//
// ── WAL transaction methods ────────────────────────────────────────────────────
//
//   begin_wal_transaction / commit_wal_transaction support crash-safe
//   multi-record commits.  Adapters that implement them write BEGIN_TXN
//   and COMMIT_TXN marker records; on WAL replay any records between a
//   BEGIN_TXN without a matching COMMIT_TXN are discarded (rolled back).
//
//   The default implementations are no-ops so existing adapters compile
//   unchanged.  Only BinaryFileStorage currently provides full WAL markers.

use crate::core::{
    edge::{Edge, EdgeId},
    error::GraphError,
    node::{Node, NodeId},
};

pub trait StoragePort: Send {
    // ── Single-record mutations ───────────────────────────────────────────────

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

    // ── Full scans (startup / compaction input) ───────────────────────────────

    /// Scan all live nodes (used on startup to rebuild the engine and seed IDs).
    fn load_all_nodes(&self) -> Result<Vec<Node>, GraphError>;

    /// Scan all live edges (used on startup to rebuild the engine).
    fn load_all_edges(&self) -> Result<Vec<Edge>, GraphError>;

    // ── Maintenance ───────────────────────────────────────────────────────────

    /// Rewrite the backing store containing only the current live state.
    /// WAL-style adapters accumulate tombstones and overwrites; compaction
    /// discards them, keeping file size proportional to live data.
    fn compact(&mut self) -> Result<(), GraphError>;

    /// Approximate size of the WAL on disk in bytes.
    /// Used to decide when to auto-compact.
    /// Default: returns 0 (not all adapters report this).
    fn wal_size_bytes(&self) -> u64 { 0 }

    // ── Crash-safe WAL transaction markers ───────────────────────────────────
    //
    // These methods wrap a batch of writes in BEGIN_TXN / COMMIT_TXN markers.
    // If the process dies between the two markers, the batch is discarded on
    // the next replay.  Without these markers a partial commit can be replayed.
    //
    // Default implementations are no-ops — adapters that do not support WAL
    // markers still compile and work; they just lose crash-safety for batches.

    /// Write a BEGIN_TXN marker.  Call before a batch of related mutations.
    fn begin_wal_transaction(&mut self, _txn_id: u64) -> Result<(), GraphError> {
        Ok(())
    }

    /// Write a COMMIT_TXN marker.  Call after all mutations in the batch.
    /// A crash between begin and commit causes the whole batch to be discarded
    /// on replay.
    fn commit_wal_transaction(&mut self, _txn_id: u64) -> Result<(), GraphError> {
        Ok(())
    }

    /// Write a ROLLBACK_TXN marker.  The records written after the matching
    /// BEGIN_TXN are discarded on the next replay.
    fn rollback_wal_transaction(&mut self, _txn_id: u64) -> Result<(), GraphError> {
        Ok(())
    }
}
