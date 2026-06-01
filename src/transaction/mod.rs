// Transaction — client-side write buffer with commit / rollback semantics.
//
// ── What a transaction does ───────────────────────────────────────────────────
//
//   begin_transaction()          → Transaction (empty buffer)
//   txn.stage_insert_node(...)   → NodeId (allocated immediately from snapshot)
//   txn.stage_insert_edge(...)   → EdgeId
//   txn.stage_delete_node(id)
//   txn.stage_delete_edge(id)
//   db.commit_transaction(txn)   → CommitResult  (all ops applied in order)
//   db.rollback_transaction(txn) → ()            (buffer discarded, IDs "wasted")
//
// ── ID allocation ─────────────────────────────────────────────────────────────
//
// The transaction clones the database's IdGenerator at begin time.  This means
// IDs are available immediately when an operation is staged — you can insert
// node Alice, get her ID, and insert an edge from Alice to Bob in the same
// transaction without committing in between.
//
//   let mut txn = db.begin_transaction();
//   let alice = txn.stage_insert_node("Person", props);
//   let bob   = txn.stage_insert_node("Person", props);
//   txn.stage_insert_edge(alice, bob, "KNOWS", 1.0, props);
//   db.commit_transaction(txn)?;
//
// ── Atomicity guarantee ───────────────────────────────────────────────────────
//
// This implementation is a *client-side buffer*, not a full WAL transaction.
// Operations are applied one at a time on commit.  If the process crashes
// mid-commit, partially-applied changes will be in the WAL.
//
// To make transactions fully crash-safe, add BEGIN_TXN / COMMIT_TXN markers
// to the WAL adapters so the replay logic can discard incomplete transactions.
// See docs/14_transactions.md Part 6 for the full implementation sketch.
//
// ── Rollback semantics ────────────────────────────────────────────────────────
//
// Rollback discards the buffer — nothing is written to storage, cache, or
// engine.  However, the IDs allocated within the transaction are "consumed"
// (the database's ID generator is advanced past them) to prevent future
// inserts from reusing those IDs.

use std::collections::HashMap;

use crate::core::{
    edge::{Edge, EdgeId},
    id_generator::IdGenerator,
    node::{Node, NodeId},
    value::Value,
};

// ── Staged operations ─────────────────────────────────────────────────────────

/// One buffered mutation waiting to be applied on commit.
pub enum StagedOp {
    InsertNode { node: Node },
    InsertEdge { edge: Edge },
    DeleteNode { id: NodeId },
    DeleteEdge { id: EdgeId },
}

// ── Transaction ───────────────────────────────────────────────────────────────

pub struct Transaction {
    operations: Vec<StagedOp>,
    /// Snapshot of the database's ID generator at begin time.
    /// Advances as operations are staged.
    id_gen: IdGenerator,
}

impl Transaction {
    /// Called by LayeredGraphDatabase::begin_transaction — not public API.
    pub(crate) fn new(id_gen_snapshot: IdGenerator) -> Self {
        Self { operations: Vec::new(), id_gen: id_gen_snapshot }
    }

    // ── Staging ───────────────────────────────────────────────────────────────

    /// Buffer a node insertion.  Returns the NodeId that will be assigned
    /// on commit — usable immediately in subsequent staging calls.
    pub fn stage_insert_node(
        &mut self,
        label: impl Into<String>,
        properties: HashMap<String, Value>,
    ) -> NodeId {
        let id = self.id_gen.next_node_id();
        self.operations.push(StagedOp::InsertNode {
            node: Node { id, label: label.into(), properties },
        });
        id
    }

    /// Buffer an edge insertion.  The source and target must exist in the
    /// database *or* have been staged for insertion earlier in this transaction.
    pub fn stage_insert_edge(
        &mut self,
        source: NodeId,
        target: NodeId,
        label: impl Into<String>,
        weight: f64,
        properties: HashMap<String, Value>,
    ) -> EdgeId {
        let id = self.id_gen.next_edge_id();
        self.operations.push(StagedOp::InsertEdge {
            edge: Edge { id, source, target, label: label.into(), weight, properties },
        });
        id
    }

    /// Buffer a node deletion.
    pub fn stage_delete_node(&mut self, id: NodeId) {
        self.operations.push(StagedOp::DeleteNode { id });
    }

    /// Buffer an edge deletion.
    pub fn stage_delete_edge(&mut self, id: EdgeId) {
        self.operations.push(StagedOp::DeleteEdge { id });
    }

    /// How many operations have been staged.
    pub fn operation_count(&self) -> usize {
        self.operations.len()
    }

    // ── Called by LayeredGraphDatabase on commit / rollback ───────────────────

    /// Consume the transaction, yielding its buffered operations.
    pub(crate) fn into_operations(self) -> Vec<StagedOp> {
        self.operations
    }

    /// Advance `target` past all IDs allocated in this transaction.
    /// Called on rollback so the database never reuses an ID.
    pub(crate) fn seed_generator_into(self, target: &mut IdGenerator) {
        for op in &self.operations {
            match op {
                StagedOp::InsertNode { node } => target.seed_from_node(node.id),
                StagedOp::InsertEdge { edge } => target.seed_from_edge(edge.id),
                _ => {}
            }
        }
    }
}

// ── CommitResult ──────────────────────────────────────────────────────────────

/// The IDs of every record affected by a committed transaction.
#[derive(Debug, Default)]
pub struct CommitResult {
    pub inserted_node_ids: Vec<NodeId>,
    pub inserted_edge_ids: Vec<EdgeId>,
    pub deleted_node_ids:  Vec<NodeId>,
    pub deleted_edge_ids:  Vec<EdgeId>,
}

impl CommitResult {
    pub fn is_empty(&self) -> bool {
        self.inserted_node_ids.is_empty()
            && self.inserted_edge_ids.is_empty()
            && self.deleted_node_ids.is_empty()
            && self.deleted_edge_ids.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_gen() -> IdGenerator { IdGenerator::new() }

    #[test]
    fn staging_node_returns_id_immediately() {
        let mut txn = Transaction::new(empty_gen());
        let id = txn.stage_insert_node("City", HashMap::new());
        assert_eq!(id, NodeId(0));
    }

    #[test]
    fn multiple_staged_nodes_get_sequential_ids() {
        let mut txn = Transaction::new(empty_gen());
        let a = txn.stage_insert_node("City", HashMap::new());
        let b = txn.stage_insert_node("City", HashMap::new());
        let c = txn.stage_insert_node("City", HashMap::new());
        assert_eq!(a, NodeId(0));
        assert_eq!(b, NodeId(1));
        assert_eq!(c, NodeId(2));
    }

    #[test]
    fn staged_edge_id_follows_node_ids() {
        let mut txn = Transaction::new(empty_gen());
        let a = txn.stage_insert_node("A", HashMap::new());
        let b = txn.stage_insert_node("B", HashMap::new());
        let eid = txn.stage_insert_edge(a, b, "LINK", 1.0, HashMap::new());
        assert_eq!(eid, EdgeId(0));
    }

    #[test]
    fn operation_count_tracks_stages() {
        let mut txn = Transaction::new(empty_gen());
        assert_eq!(txn.operation_count(), 0);
        txn.stage_insert_node("A", HashMap::new());
        txn.stage_insert_node("B", HashMap::new());
        assert_eq!(txn.operation_count(), 2);
        txn.stage_delete_node(NodeId(99));
        assert_eq!(txn.operation_count(), 3);
    }

    #[test]
    fn seed_generator_into_advances_past_allocated_ids() {
        let mut txn = Transaction::new(empty_gen());
        txn.stage_insert_node("A", HashMap::new()); // allocates N0
        txn.stage_insert_node("B", HashMap::new()); // allocates N1

        let mut gen = IdGenerator::new();
        txn.seed_generator_into(&mut gen);
        // generator must now produce N2, not reuse N0/N1
        assert_eq!(gen.next_node_id(), NodeId(2));
    }
}
