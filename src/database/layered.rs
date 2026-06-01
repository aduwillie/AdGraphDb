// LayeredGraphDatabase — the assembled application.
//
// This is the only place where port adapters meet.
// All external code interacts with this struct; it never holds concrete adapter
// types (only Box<dyn Port>), so every adapter can be swapped at construction.
//
// ── Layers ────────────────────────────────────────────────────────────────────
//
//   Engine  (GraphEnginePort)
//     In-memory adjacency index.  Built from storage on open().
//     Answers structural queries (neighbors, traversal, shortest path)
//     without touching disk.
//
//   LabelIndex
//     Secondary in-memory index: label → Vec<NodeId>.
//     Converts O(N) label scans to O(1) lookups.
//     Kept in sync on every insert/delete alongside the engine.
//
//   Cache   (CachePort)
//     In-memory store for node/edge property data.
//     Consulted before storage on every read; populated on cache miss.
//     Invalidated on write/delete to prevent stale reads.
//
//   Storage (StoragePort)
//     Durable write-ahead log on disk.
//     Written on every mutation before the cache is updated.
//
// ── Write path ────────────────────────────────────────────────────────────────
//
//   insert_node(label, props):
//     1. storage.save_node(&node)   ← durable first
//     2. cache.put_node(node)
//     3. engine.insert_node(id)
//     4. label_index.insert(id, label)  ← keep index in sync
//
// ── Read path (optimized) ─────────────────────────────────────────────────────
//
//   MATCH NODE WHERE label = "City"    (before: O(N), now: O(label_count))
//     1. label_index.get("City")       ← O(1) HashMap lookup
//     2. For each id in the result:
//          get_node(id)                ← cache hit or storage miss
//     3. Apply property conditions
//
//   MATCH NODE (no label filter)       still O(N) — full scan needed
//     1. engine.all_node_ids()
//     2. get_node(id) for each
//     3. Apply conditions

use std::collections::HashMap;

use crate::adapters::index::label_index::LabelIndex;
use crate::algorithms::{bfs::BreadthFirstSearch, dfs::DepthFirstSearch, dijkstra::Dijkstra};
use crate::core::{
    edge::{Edge, EdgeId},
    error::GraphError,
    id_generator::IdGenerator,
    node::{Node, NodeId},
    value::Value,
};
use crate::ports::{
    algorithm::{ShortestPathAlgorithm, TraversalAlgorithm},
    cache::CachePort,
    engine::GraphEnginePort,
    query_context::DatabaseContext,
    storage::StoragePort,
};
use crate::query::{port::QueryLanguagePort, result::QueryResult};
use crate::transaction::{CommitResult, Transaction};

pub struct LayeredGraphDatabase {
    engine:       Box<dyn GraphEnginePort>,
    cache:        Box<dyn CachePort>,
    storage:      Box<dyn StoragePort>,
    label_index:  LabelIndex,
    id_generator: IdGenerator,
}

impl LayeredGraphDatabase {
    /// Construct the database, replaying the WAL to rebuild all in-memory state.
    ///
    /// The cache starts cold.  The engine and label index are rebuilt from the
    /// full node+edge record set — no separate index file is needed.
    pub fn open(
        storage: Box<dyn StoragePort>,
        cache:   Box<dyn CachePort>,
        mut engine: Box<dyn GraphEnginePort>,
    ) -> Result<Self, GraphError> {
        let mut id_gen = IdGenerator::new();
        let mut label_index = LabelIndex::new();

        // load_all_nodes() returns full Node structs (label included),
        // so we can populate both engine and label_index in one pass.
        for node in storage.load_all_nodes()? {
            id_gen.seed_from_node(node.id);
            engine.insert_node(node.id);
            label_index.insert(node.id, &node.label);
        }

        for edge in storage.load_all_edges()? {
            id_gen.seed_from_edge(edge.id);
            engine.insert_edge(edge.id, edge.source, edge.target, edge.weight);
        }

        Ok(Self { engine, cache, storage, label_index, id_generator: id_gen })
    }

    // ── Node operations ───────────────────────────────────────────────────────

    pub fn insert_node(
        &mut self,
        label: impl Into<String>,
        properties: HashMap<String, Value>,
    ) -> Result<NodeId, GraphError> {
        let id = self.id_generator.next_node_id();
        let node = Node { id, label: label.into(), properties };

        self.storage.save_node(&node)?;
        self.cache.put_node(node.clone());
        self.engine.insert_node(id);
        self.label_index.insert(id, &node.label);

        Ok(id)
    }

    pub fn get_node(&mut self, id: NodeId) -> Result<Option<Node>, GraphError> {
        if let Some(node) = self.cache.get_node(id) {
            return Ok(Some(node));
        }
        if let Some(node) = self.storage.load_node(id)? {
            self.cache.put_node(node.clone());
            return Ok(Some(node));
        }
        Ok(None)
    }

    pub fn delete_node(&mut self, id: NodeId) -> Result<(), GraphError> {
        // Remove incident edges from storage and cache first.
        let outgoing: Vec<EdgeId> = self
            .engine.neighbors_outgoing(id)
            .into_iter().map(|n| n.edge_id).collect();
        let incoming: Vec<EdgeId> = self
            .engine.neighbors_incoming(id)
            .into_iter().map(|n| n.edge_id).collect();

        for eid in outgoing.into_iter().chain(incoming) {
            self.storage.delete_edge(eid)?;
            self.cache.invalidate_edge(eid);
        }

        // Remove the node from storage, cache, engine, and label index.
        // To remove from the label index we need the label — load it first.
        if let Some(node) = self.get_node(id)? {
            self.label_index.remove(id, &node.label);
        }

        self.storage.delete_node(id)?;
        self.cache.invalidate_node(id);
        self.engine.remove_node(id);

        Ok(())
    }

    // ── Edge operations ───────────────────────────────────────────────────────

    pub fn insert_edge(
        &mut self,
        source: NodeId,
        target: NodeId,
        label: impl Into<String>,
        weight: f64,
        properties: HashMap<String, Value>,
    ) -> Result<EdgeId, GraphError> {
        if !self.engine.contains_node(source) {
            return Err(GraphError::NodeNotFound(source));
        }
        if !self.engine.contains_node(target) {
            return Err(GraphError::NodeNotFound(target));
        }

        let id = self.id_generator.next_edge_id();
        let edge = Edge { id, source, target, label: label.into(), weight, properties };

        self.storage.save_edge(&edge)?;
        self.cache.put_edge(edge);
        self.engine.insert_edge(id, source, target, weight);

        Ok(id)
    }

    pub fn get_edge(&mut self, id: EdgeId) -> Result<Option<Edge>, GraphError> {
        if let Some(edge) = self.cache.get_edge(id) {
            return Ok(Some(edge));
        }
        if let Some(edge) = self.storage.load_edge(id)? {
            self.cache.put_edge(edge.clone());
            return Ok(Some(edge));
        }
        Ok(None)
    }

    pub fn delete_edge(&mut self, id: EdgeId) -> Result<(), GraphError> {
        self.storage.delete_edge(id)?;
        self.cache.invalidate_edge(id);
        self.engine.remove_edge(id);
        Ok(())
    }

    // ── Structural queries ────────────────────────────────────────────────────

    pub fn neighbors_outgoing(&self, id: NodeId) -> Vec<crate::ports::engine::Neighbor> {
        self.engine.neighbors_outgoing(id)
    }

    pub fn neighbors_incoming(&self, id: NodeId) -> Vec<crate::ports::engine::Neighbor> {
        self.engine.neighbors_incoming(id)
    }

    pub fn node_count(&self) -> usize { self.engine.node_count() }
    pub fn edge_count(&self) -> usize { self.engine.edge_count() }
    pub fn all_node_ids(&self) -> Vec<NodeId> { self.engine.all_node_ids() }
    pub fn all_edge_ids(&self) -> Vec<EdgeId> { self.engine.all_edge_ids() }

    // ── Label index queries ───────────────────────────────────────────────────

    /// All node IDs that carry a specific label.
    /// O(1) — single HashMap lookup into the label index.
    pub fn node_ids_by_label(&self, label: &str) -> &[NodeId] {
        self.label_index.get(label)
    }

    /// How many nodes carry a specific label.
    pub fn label_count(&self, label: &str) -> usize {
        self.label_index.label_count(label)
    }

    // ── Algorithm dispatch ────────────────────────────────────────────────────

    pub fn traverse(
        &self,
        algorithm: &dyn TraversalAlgorithm,
        start: NodeId,
    ) -> Vec<NodeId> {
        algorithm.traverse(self.engine.as_ref(), start)
    }

    pub fn find_shortest_path(
        &self,
        algorithm: &dyn ShortestPathAlgorithm,
        start: NodeId,
        goal: NodeId,
    ) -> Option<(Vec<NodeId>, f64)> {
        algorithm.find_shortest_path(self.engine.as_ref(), start, goal)
    }

    // ── Query language execution ──────────────────────────────────────────────

    pub fn execute_query(
        &mut self,
        language: &dyn QueryLanguagePort,
        query: &str,
    ) -> Result<QueryResult, GraphError> {
        language.execute(query, self)
    }

    // ── Maintenance ───────────────────────────────────────────────────────────

    pub fn compact(&mut self) -> Result<(), GraphError> {
        self.storage.compact()?;
        self.cache.clear();
        Ok(())
    }

    // ── Bulk helpers (used by DatabaseContext impl and all_nodes/all_edges) ───

    pub fn all_nodes(&mut self) -> Result<Vec<Node>, GraphError> {
        let ids = self.engine.all_node_ids();
        let mut nodes = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(node) = self.get_node(id)? {
                nodes.push(node);
            }
        }
        Ok(nodes)
    }

    /// Load nodes for a specific label — uses the label index (O(label_count)).
    pub fn nodes_by_label(&mut self, label: &str) -> Result<Vec<Node>, GraphError> {
        let ids: Vec<NodeId> = self.label_index.get(label).to_vec();
        let mut nodes = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(node) = self.get_node(id)? {
                nodes.push(node);
            }
        }
        Ok(nodes)
    }

    pub fn all_edges(&mut self) -> Result<Vec<Edge>, GraphError> {
        let ids = self.engine.all_edge_ids();
        let mut edges = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(edge) = self.get_edge(id)? {
                edges.push(edge);
            }
        }
        Ok(edges)
    }

    // ── Transactions ──────────────────────────────────────────────────────────

    /// Begin a transaction.  Operations staged on the returned value are
    /// buffered in memory.  Call `commit_transaction` to apply them all
    /// or `rollback_transaction` to discard them.
    ///
    /// The transaction receives a clone of the ID generator so it can
    /// allocate IDs immediately — callers can reference inserted node IDs
    /// in subsequent operations within the same transaction
    /// (e.g. insert an edge between two nodes that don't exist yet).
    pub fn begin_transaction(&mut self) -> Transaction {
        Transaction::new(self.id_generator.clone())
    }

    /// Apply all staged operations to storage, cache, engine, and label index.
    ///
    /// Operations are applied in the order they were staged.  If any storage
    /// write fails partway through, the changes applied so far are NOT rolled
    /// back automatically — this is a client-side buffer, not a full ACID
    /// transaction.  See docs/15_rust_concepts.md for the WAL-transaction
    /// extension that would make this crash-safe.
    pub fn commit_transaction(
        &mut self,
        txn: Transaction,
    ) -> Result<CommitResult, GraphError> {
        let mut result = CommitResult::default();

        for op in txn.into_operations() {
            use crate::transaction::StagedOp;
            match op {
                StagedOp::InsertNode { node } => {
                    let id = node.id;
                    let label = node.label.clone();
                    self.storage.save_node(&node)?;
                    self.cache.put_node(node);
                    self.engine.insert_node(id);
                    self.label_index.insert(id, &label);
                    self.id_generator.seed_from_node(id);
                    result.inserted_node_ids.push(id);
                }
                StagedOp::InsertEdge { edge } => {
                    let id = edge.id;
                    if !self.engine.contains_node(edge.source) {
                        return Err(GraphError::NodeNotFound(edge.source));
                    }
                    if !self.engine.contains_node(edge.target) {
                        return Err(GraphError::NodeNotFound(edge.target));
                    }
                    self.engine.insert_edge(id, edge.source, edge.target, edge.weight);
                    self.storage.save_edge(&edge)?;
                    self.cache.put_edge(edge);
                    self.id_generator.seed_from_edge(id);
                    result.inserted_edge_ids.push(id);
                }
                StagedOp::DeleteNode { id } => {
                    self.delete_node(id)?;
                    result.deleted_node_ids.push(id);
                }
                StagedOp::DeleteEdge { id } => {
                    self.delete_edge(id)?;
                    result.deleted_edge_ids.push(id);
                }
            }
        }

        Ok(result)
    }

    /// Discard all staged operations.
    ///
    /// Because IDs are allocated eagerly inside the transaction, this call
    /// must advance the database's ID generator past any IDs that were
    /// assigned — otherwise the next non-transaction insert could reuse them.
    pub fn rollback_transaction(&mut self, txn: Transaction) {
        // Advance the generator past all IDs the transaction allocated,
        // even though no data was written to storage, cache, or engine.
        txn.seed_generator_into(&mut self.id_generator);
    }
}

// ── DatabaseContext implementation ────────────────────────────────────────────

impl DatabaseContext for LayeredGraphDatabase {
    fn get_node(&mut self, id: NodeId) -> Result<Option<Node>, GraphError> {
        self.get_node(id)
    }

    fn get_all_nodes(&mut self) -> Result<Vec<Node>, GraphError> {
        self.all_nodes()
    }

    /// Uses the label index — O(label_count) instead of O(N).
    fn get_nodes_by_label(&mut self, label: &str) -> Result<Vec<Node>, GraphError> {
        self.nodes_by_label(label)
    }

    fn get_edge(&mut self, id: EdgeId) -> Result<Option<Edge>, GraphError> {
        self.get_edge(id)
    }

    fn get_all_edges(&mut self) -> Result<Vec<Edge>, GraphError> {
        self.all_edges()
    }

    fn node_count(&self) -> usize { self.node_count() }
    fn edge_count(&self) -> usize { self.edge_count() }

    fn traverse_bfs(&self, start: NodeId) -> Vec<NodeId> {
        self.traverse(&BreadthFirstSearch, start)
    }

    fn traverse_dfs(&self, start: NodeId) -> Vec<NodeId> {
        self.traverse(&DepthFirstSearch, start)
    }

    fn shortest_path_dijkstra(&self, start: NodeId, goal: NodeId) -> Option<(Vec<NodeId>, f64)> {
        self.find_shortest_path(&Dijkstra, start, goal)
    }
}
