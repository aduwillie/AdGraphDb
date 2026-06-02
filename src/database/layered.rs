// LayeredGraphDatabase — the assembled application.
//
// Holds all adapters and indexes; exposes the public database API and
// implements DatabaseContext so query executors can call it directly.
//
// ── Layers (inner → outer) ────────────────────────────────────────────────────
//
//   Storage (disk WAL)      — durable, written first on every mutation
//   Cache   (RAM, LRU)      — property data for hot nodes/edges, lazy-loaded
//   Engine  (RAM adjacency) — graph structure: who connects to whom + weights
//   LabelIndex (RAM)        — label → Vec<NodeId>, O(1) label queries
//   PropertyIndex (RAM)     — field → BTree → Vec<NodeId>, O(log N) property queries
//   Metrics (RAM counters)  — query/cache/index statistics
//   Config  (immutable)     — tuning parameters (auto-compact threshold, etc.)
//
// ── Write path ────────────────────────────────────────────────────────────────
//
//   insert_node(label, props):
//     1. storage.save_node()       durable first
//     2. cache.put_node()          RAM property store
//     3. engine.insert_node()      adjacency index
//     4. label_index.insert()      label secondary index
//     5. property_index.insert()   property secondary index
//     6. maybe_auto_compact()      compact WAL if threshold exceeded
//
// ── Read path ─────────────────────────────────────────────────────────────────
//
//   Query planner chooses the cheapest strategy:
//     PropertyIndexScan  O(log N + results)  — for indexed property conditions
//     LabelIndexScan     O(label_count)       — for label filters (always fast)
//     FullNodeScan       O(N)                 — fallback, no applicable index
//
//   For point lookups: cache → storage (O(1) hit, O(WAL) miss)
//   For traversals:    engine only (O(V+E) in RAM, no I/O)

use std::collections::HashMap;

use crate::adapters::index::{
    label_index::LabelIndex,
    property_index::PropertyIndex,
};
use crate::algorithms::{bfs::BreadthFirstSearch, dfs::DepthFirstSearch, dijkstra::Dijkstra};
use crate::core::{
    edge::{Edge, EdgeId},
    error::GraphError,
    id_generator::IdGenerator,
    node::{Node, NodeId},
    value::Value,
};
use crate::database::{
    config::DatabaseConfig,
    metrics::DatabaseMetrics,
};
use crate::ports::{
    algorithm::{ShortestPathAlgorithm, TraversalAlgorithm},
    cache::CachePort,
    engine::GraphEnginePort,
    query_context::DatabaseContext,
    storage::StoragePort,
};
use crate::query::{
    ast::ComparisonOp,
    planner::DatabaseStats,
    port::QueryLanguagePort,
    result::QueryResult,
    executor::execute_with_explain,
};
use crate::transaction::{CommitResult, Transaction};

// ── Main struct ───────────────────────────────────────────────────────────────

pub struct LayeredGraphDatabase {
    engine:          Box<dyn GraphEnginePort>,
    cache:           Box<dyn CachePort>,
    storage:         Box<dyn StoragePort>,
    label_index:     LabelIndex,
    property_index:  PropertyIndex,
    id_generator:    IdGenerator,
    config:          DatabaseConfig,
    metrics:         DatabaseMetrics,
    /// Counts writes since the last compaction (or since open).
    writes_since_compact: usize,
}

impl LayeredGraphDatabase {

    // ── Constructors ──────────────────────────────────────────────────────────

    /// Open with default configuration.
    pub fn open(
        storage: Box<dyn StoragePort>,
        cache:   Box<dyn CachePort>,
        engine:  Box<dyn GraphEnginePort>,
    ) -> Result<Self, GraphError> {
        Self::open_with_config(storage, cache, engine, DatabaseConfig::default())
    }

    /// Open with explicit configuration.
    pub fn open_with_config(
        storage:    Box<dyn StoragePort>,
        cache:      Box<dyn CachePort>,
        mut engine: Box<dyn GraphEnginePort>,
        config:     DatabaseConfig,
    ) -> Result<Self, GraphError> {
        let mut id_gen         = IdGenerator::new();
        let mut label_index    = LabelIndex::new();
        let mut property_index = PropertyIndex::new();

        // Replay WAL: rebuild engine, label index, and property index in one pass.
        for node in storage.load_all_nodes()? {
            id_gen.seed_from_node(node.id);
            engine.insert_node(node.id);
            label_index.insert(node.id, &node.label);
            if config.indexes_all_fields() {
                property_index.insert_node(node.id, &node.properties);
            } else {
                let filtered: HashMap<String, Value> = node.properties.iter()
                    .filter(|(k, _)| config.should_index_field(k))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                if !filtered.is_empty() {
                    property_index.insert_node(node.id, &filtered);
                }
            }
        }

        for edge in storage.load_all_edges()? {
            id_gen.seed_from_edge(edge.id);
            engine.insert_edge(edge.id, edge.source, edge.target, edge.weight);
        }

        Ok(Self {
            engine,
            cache,
            storage,
            label_index,
            property_index,
            id_generator:         id_gen,
            config,
            metrics:              DatabaseMetrics::new(),
            writes_since_compact: 0,
        })
    }

    // ── Node operations ───────────────────────────────────────────────────────

    pub fn insert_node(
        &mut self,
        label:      impl Into<String>,
        properties: HashMap<String, Value>,
    ) -> Result<NodeId, GraphError> {
        let id   = self.id_generator.next_node_id();
        let node = Node { id, label: label.into(), properties };

        self.storage.save_node(&node)?;
        self.cache.put_node(node.clone());
        self.engine.insert_node(id);
        self.label_index.insert(id, &node.label);
        if self.config.indexes_all_fields() {
            self.property_index.insert_node(id, &node.properties);
        } else {
            let filtered: HashMap<String, Value> = node.properties.iter()
                .filter(|(k, _)| self.config.should_index_field(k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if !filtered.is_empty() {
                self.property_index.insert_node(id, &filtered);
            }
        }
        self.metrics.nodes_inserted += 1;
        self.on_write();
        Ok(id)
    }

    pub fn get_node(&mut self, id: NodeId) -> Result<Option<Node>, GraphError> {
        if let Some(node) = self.cache.get_node(id) {
            self.metrics.cache_node_hits += 1;
            return Ok(Some(node));
        }
        self.metrics.cache_node_misses += 1;
        if let Some(node) = self.storage.load_node(id)? {
            self.cache.put_node(node.clone());
            return Ok(Some(node));
        }
        Ok(None)
    }

    pub fn delete_node(&mut self, id: NodeId) -> Result<(), GraphError> {
        // Collect incident edge IDs from the engine before removing.
        let incident: Vec<EdgeId> = self.engine.neighbors_outgoing(id)
            .into_iter().map(|n| n.edge_id)
            .chain(self.engine.neighbors_incoming(id).into_iter().map(|n| n.edge_id))
            .collect();

        for eid in incident {
            self.storage.delete_edge(eid)?;
            self.cache.invalidate_edge(eid);
        }

        // Need the node's label and properties to update the indexes.
        if let Some(node) = self.get_node(id)? {
            self.label_index.remove(id, &node.label);
            self.property_index.remove_node(id, &node.properties);
        }

        self.storage.delete_node(id)?;
        self.cache.invalidate_node(id);
        self.engine.remove_node(id);

        self.metrics.nodes_deleted += 1;
        self.on_write();
        Ok(())
    }

    // ── Edge operations ───────────────────────────────────────────────────────

    pub fn insert_edge(
        &mut self,
        source:     NodeId,
        target:     NodeId,
        label:      impl Into<String>,
        weight:     f64,
        properties: HashMap<String, Value>,
    ) -> Result<EdgeId, GraphError> {
        if !self.engine.contains_node(source) {
            return Err(GraphError::NodeNotFound(source));
        }
        if !self.engine.contains_node(target) {
            return Err(GraphError::NodeNotFound(target));
        }

        let id   = self.id_generator.next_edge_id();
        let edge = Edge { id, source, target, label: label.into(), weight, properties };

        self.storage.save_edge(&edge)?;
        self.cache.put_edge(edge.clone());
        self.engine.insert_edge(id, source, target, weight);

        self.metrics.edges_inserted += 1;
        self.on_write();
        Ok(id)
    }

    pub fn get_edge(&mut self, id: EdgeId) -> Result<Option<Edge>, GraphError> {
        if let Some(edge) = self.cache.get_edge(id) {
            self.metrics.cache_edge_hits += 1;
            return Ok(Some(edge));
        }
        self.metrics.cache_edge_misses += 1;
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
        self.metrics.edges_deleted += 1;
        self.on_write();
        Ok(())
    }

    // ── Bulk helpers ──────────────────────────────────────────────────────────

    pub fn all_nodes(&mut self) -> Result<Vec<Node>, GraphError> {
        let ids = self.engine.all_node_ids();
        let mut nodes = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(node) = self.get_node(id)? { nodes.push(node); }
        }
        Ok(nodes)
    }

    pub fn nodes_by_label(&mut self, label: &str) -> Result<Vec<Node>, GraphError> {
        let ids: Vec<NodeId> = self.label_index.get(label).to_vec();
        let mut nodes = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(node) = self.get_node(id)? { nodes.push(node); }
        }
        Ok(nodes)
    }

    /// Return nodes where `field <op> value`, using the PropertyIndex when available.
    pub fn nodes_by_property(
        &mut self,
        field: &str,
        op:    &ComparisonOp,
        value: &Value,
    ) -> Result<Vec<Node>, GraphError> {
        let ids_opt = self.property_index.query(field, op, value);
        match ids_opt {
            Some(ids) => {
                // Index hit: load only the matching subset.
                self.metrics.property_index_hits += 1;
                let mut nodes = Vec::with_capacity(ids.len());
                for id in ids {
                    if let Some(node) = self.get_node(id)? { nodes.push(node); }
                }
                Ok(nodes)
            }
            None => {
                // No index for this field — fall back to full scan.
                self.metrics.full_node_scans += 1;
                let all = self.all_nodes()?;
                Ok(all.into_iter().filter(|n| {
                    n.properties.get(field)
                        .map(|v| op.compare_values(v, value))
                        .unwrap_or(false)
                }).collect())
            }
        }
    }

    pub fn all_edges(&mut self) -> Result<Vec<Edge>, GraphError> {
        let ids = self.engine.all_edge_ids();
        let mut edges = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(edge) = self.get_edge(id)? { edges.push(edge); }
        }
        Ok(edges)
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

    pub fn node_ids_by_label(&self, label: &str) -> &[NodeId] { self.label_index.get(label) }
    pub fn label_count(&self, label: &str) -> usize { self.label_index.label_count(label) }

    // ── Algorithm dispatch ────────────────────────────────────────────────────

    pub fn traverse(&self, algorithm: &dyn TraversalAlgorithm, start: NodeId) -> Vec<NodeId> {
        algorithm.traverse(self.engine.as_ref(), start)
    }

    pub fn find_shortest_path(
        &self,
        algorithm: &dyn ShortestPathAlgorithm,
        start: NodeId,
        goal:  NodeId,
    ) -> Option<(Vec<NodeId>, f64)> {
        algorithm.find_shortest_path(self.engine.as_ref(), start, goal)
    }

    // ── Query execution ───────────────────────────────────────────────────────

    pub fn execute_query(
        &mut self,
        language: &dyn QueryLanguagePort,
        query:    &str,
    ) -> Result<QueryResult, GraphError> {
        let start = std::time::Instant::now();
        let result = language.execute(query, self);
        let elapsed_ns = start.elapsed().as_nanos() as u64;
        self.metrics.queries_executed    += 1;
        self.metrics.total_query_time_ns += elapsed_ns;

        if let Some(threshold_ms) = self.config.slow_query_warn_ms {
            if elapsed_ns / 1_000_000 >= threshold_ms {
                eprintln!("[AdGraphDb] slow query ({} ms): {query}",
                    elapsed_ns / 1_000_000);
            }
        }
        result
    }

    /// Execute and return a human-readable description of the chosen plan.
    pub fn execute_query_with_explain(
        &mut self,
        language: &dyn QueryLanguagePort,
        query:    &str,
    ) -> Result<(QueryResult, String), GraphError> {
        let stats    = self.build_stats();
        let start    = std::time::Instant::now();
        let result   = language.execute(query, self)?;
        let elapsed_ns = start.elapsed().as_nanos() as u64;
        self.metrics.queries_executed    += 1;
        self.metrics.total_query_time_ns += elapsed_ns;
        let plan_desc = format!(
            "stats: {} nodes, {} labeled, {} indexed fields  |  elapsed: {} µs",
            stats.node_count,
            stats.label_counts.values().sum::<usize>(),
            stats.indexed_node_fields.len(),
            elapsed_ns / 1000,
        );
        Ok((result, plan_desc))
    }

    // ── Maintenance ───────────────────────────────────────────────────────────

    pub fn compact(&mut self) -> Result<(), GraphError> {
        self.storage.compact()?;
        self.cache.clear();
        self.metrics.compactions += 1;
        self.writes_since_compact = 0;
        Ok(())
    }

    // ── Statistics and metrics ────────────────────────────────────────────────

    pub fn metrics(&self) -> &DatabaseMetrics { &self.metrics }
    pub fn reset_metrics(&mut self) { self.metrics = DatabaseMetrics::new(); }

    pub fn config(&self) -> &DatabaseConfig { &self.config }

    fn build_stats(&self) -> DatabaseStats {
        let mut label_counts = std::collections::HashMap::new();
        for label in self.label_index.all_labels() {
            label_counts.insert(label.to_string(), self.label_index.label_count(label));
        }
        let indexed_node_fields = self.property_index.indexed_fields()
            .map(|s| s.to_string())
            .collect();
        DatabaseStats {
            node_count:          self.engine.node_count(),
            edge_count:          self.engine.edge_count(),
            label_counts,
            indexed_node_fields,
        }
    }

    // ── Transactions ──────────────────────────────────────────────────────────

    pub fn begin_transaction(&mut self) -> Transaction {
        Transaction::new(self.id_generator.clone())
    }

    pub fn commit_transaction(
        &mut self,
        txn: Transaction,
    ) -> Result<CommitResult, GraphError> {
        let txn_id = txn.id();
        self.storage.begin_wal_transaction(txn_id)?;

        let mut result = CommitResult::default();
        let ops = txn.into_operations();

        for op in ops {
            use crate::transaction::StagedOp;
            match op {
                StagedOp::InsertNode { node } => {
                    let id = node.id;
                    let label = node.label.clone();
                    self.storage.save_node(&node)?;
                    self.cache.put_node(node.clone());
                    self.engine.insert_node(id);
                    self.label_index.insert(id, &label);
                    self.property_index.insert_node(id, &node.properties);
                    self.id_generator.seed_from_node(id);
                    self.metrics.nodes_inserted += 1;
                    result.inserted_node_ids.push(id);
                }
                StagedOp::InsertEdge { edge } => {
                    let id = edge.id;
                    if !self.engine.contains_node(edge.source) {
                        self.storage.rollback_wal_transaction(txn_id).ok();
                        return Err(GraphError::NodeNotFound(edge.source));
                    }
                    if !self.engine.contains_node(edge.target) {
                        self.storage.rollback_wal_transaction(txn_id).ok();
                        return Err(GraphError::NodeNotFound(edge.target));
                    }
                    self.storage.save_edge(&edge)?;
                    self.cache.put_edge(edge.clone());
                    self.engine.insert_edge(id, edge.source, edge.target, edge.weight);
                    self.id_generator.seed_from_edge(id);
                    self.metrics.edges_inserted += 1;
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

        self.storage.commit_wal_transaction(txn_id)?;
        self.metrics.transactions_committed += 1;
        self.on_write();
        Ok(result)
    }

    pub fn rollback_transaction(&mut self, txn: Transaction) {
        txn.seed_generator_into(&mut self.id_generator);
        self.metrics.transactions_rolled_back += 1;
    }

    // ── Auto-compaction trigger ───────────────────────────────────────────────

    fn on_write(&mut self) {
        self.writes_since_compact += 1;
        if let Some(threshold) = self.config.auto_compact_after_writes {
            if self.writes_since_compact >= threshold {
                // Best-effort: log the error but don't propagate it.
                if let Err(e) = self.compact() {
                    eprintln!("[AdGraphDb] auto-compact failed: {e}");
                }
            }
        }
    }
}

// ── DatabaseContext implementation ────────────────────────────────────────────

impl DatabaseContext for LayeredGraphDatabase {
    fn get_node(&mut self, id: NodeId) -> Result<Option<Node>, GraphError> {
        self.get_node(id)
    }

    fn get_all_nodes(&mut self) -> Result<Vec<Node>, GraphError> {
        self.metrics.full_node_scans += 1;
        self.all_nodes()
    }

    fn get_nodes_by_label(&mut self, label: &str) -> Result<Vec<Node>, GraphError> {
        self.metrics.label_index_hits += 1;
        self.nodes_by_label(label)
    }

    fn get_nodes_by_property(
        &mut self,
        field: &str,
        op:    &ComparisonOp,
        value: &Value,
    ) -> Result<Vec<Node>, GraphError> {
        self.nodes_by_property(field, op, value)
    }

    fn get_edge(&mut self, id: EdgeId) -> Result<Option<Edge>, GraphError> {
        self.get_edge(id)
    }

    fn get_all_edges(&mut self) -> Result<Vec<Edge>, GraphError> {
        self.metrics.full_edge_scans += 1;
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

    fn stats(&self) -> DatabaseStats {
        self.build_stats()
    }
}
