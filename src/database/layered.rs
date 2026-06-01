// LayeredGraphDatabase — the assembled application.
//
// This is the only place where the three port adapters meet.
// All external code interacts with this struct; it never holds concrete adapter
// types (only Box<dyn Port>), so every adapter can be swapped at construction.
//
// ── Layer responsibilities ────────────────────────────────────────────────────
//
//   Engine  (GraphEnginePort)
//     In-memory adjacency index.  Built from storage on open().
//     Answers structural queries (neighbors, traversal, shortest path)
//     without touching disk.
//
//   Cache   (CachePort)
//     In-memory store for node/edge property data.
//     Consulted before storage on every read; populated on cache miss.
//     Invalidated on write/delete to prevent stale reads.
//
//   Storage (StoragePort)
//     Durable write-ahead log on disk.
//     Written on every mutation before the cache is updated,
//     so data survives a crash.
//
// ── Read path ─────────────────────────────────────────────────────────────────
//
//   get_node(id):
//     1. cache.get_node(id)      → return if Some
//     2. storage.load_node(id)   → if Some: cache.put_node, return
//     3. return None
//
// ── Write path ────────────────────────────────────────────────────────────────
//
//   insert_node(label, props):
//     1. id_generator.next_node_id()
//     2. storage.save_node(&node)   ← durable first
//     3. cache.put_node(node)
//     4. engine.insert_node(id)
//     5. return id
//
// ── Delete path ───────────────────────────────────────────────────────────────
//
//   delete_node(id):
//     1. storage.delete_node(id)  ← durable first
//     2. cache.invalidate_node(id)
//     3. engine.remove_node(id)   ← also removes incident edges from index

use std::collections::HashMap;

use crate::core::{
    edge::{Edge, EdgeId},
    error::GraphError,
    id_generator::IdGenerator,
    node::{Node, NodeId},
    value::Value,
};
use crate::algorithms::{bfs::BreadthFirstSearch, dfs::DepthFirstSearch, dijkstra::Dijkstra};
use crate::ports::{
    algorithm::{ShortestPathAlgorithm, TraversalAlgorithm},
    cache::CachePort,
    engine::GraphEnginePort,
    query_context::DatabaseContext,
    storage::StoragePort,
};
use crate::query::{port::QueryLanguagePort, result::QueryResult};

pub struct LayeredGraphDatabase {
    engine:       Box<dyn GraphEnginePort>,
    cache:        Box<dyn CachePort>,
    storage:      Box<dyn StoragePort>,
    id_generator: IdGenerator,
}

impl LayeredGraphDatabase {
    /// Construct the database from three adapter boxes.
    ///
    /// Loads all nodes and edges from storage into the engine so structural
    /// queries work immediately without hitting the disk.
    /// The cache starts cold (populated lazily on first property read).
    pub fn open(
        storage: Box<dyn StoragePort>,
        cache:       Box<dyn CachePort>,
        mut engine:  Box<dyn GraphEnginePort>,
    ) -> Result<Self, GraphError> {
        let mut id_gen = IdGenerator::new();

        // Rebuild the structural index from durable storage.
        for node in storage.load_all_nodes()? {
            id_gen.seed_from_node(node.id);
            engine.insert_node(node.id);
        }

        for edge in storage.load_all_edges()? {
            id_gen.seed_from_edge(edge.id);
            engine.insert_edge(edge.id, edge.source, edge.target, edge.weight);
        }

        Ok(Self {
            engine,
            cache,
            storage,
            id_generator: id_gen,
        })
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
        self.cache.put_node(node);
        self.engine.insert_node(id);

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
        // Delete all edges incident to this node first (keep storage consistent).
        let outgoing_edge_ids: Vec<EdgeId> = self
            .engine
            .neighbors_outgoing(id)
            .into_iter()
            .map(|n| n.edge_id)
            .collect();

        let incoming_edge_ids: Vec<EdgeId> = self
            .engine
            .neighbors_incoming(id)
            .into_iter()
            .map(|n| n.edge_id)
            .collect();

        for eid in outgoing_edge_ids.into_iter().chain(incoming_edge_ids) {
            self.storage.delete_edge(eid)?;
            self.cache.invalidate_edge(eid);
        }

        self.storage.delete_node(id)?;
        self.cache.invalidate_node(id);
        self.engine.remove_node(id); // also cleans up the edge index entries

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

    /// Direct outgoing neighbors of a node (from the in-memory engine — no I/O).
    pub fn neighbors_outgoing(&self, id: NodeId) -> Vec<crate::ports::engine::Neighbor> {
        self.engine.neighbors_outgoing(id)
    }

    /// Direct incoming neighbors of a node.
    pub fn neighbors_incoming(&self, id: NodeId) -> Vec<crate::ports::engine::Neighbor> {
        self.engine.neighbors_incoming(id)
    }

    pub fn node_count(&self) -> usize { self.engine.node_count() }
    pub fn edge_count(&self) -> usize { self.engine.edge_count() }

    pub fn all_node_ids(&self) -> Vec<NodeId> { self.engine.all_node_ids() }
    pub fn all_edge_ids(&self) -> Vec<EdgeId> { self.engine.all_edge_ids() }

    // ── Algorithm dispatch ────────────────────────────────────────────────────
    //
    // Algorithms receive a reference to the engine (structural data only).
    // Property data is not passed — algorithms should not need it.
    // If a future algorithm does need properties, add a second method that
    // accepts &mut self and performs get_node calls inline.

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

    // ── Maintenance ───────────────────────────────────────────────────────────

    /// Rewrite the storage file to contain only live records (no tombstones).
    /// Invalidates the cache because the storage file pointer is reset.
    pub fn compact(&mut self) -> Result<(), GraphError> {
        self.storage.compact()?;
        self.cache.clear();
        Ok(())
    }

    // ── Query language execution ──────────────────────────────────────────────

    /// Execute a query written in any language that implements QueryLanguagePort.
    ///
    /// ```rust,ignore
    /// let simple  = SimpleQueryLanguage;
    /// let cypher  = CypherLiteLanguage;
    ///
    /// // Identical intent, two surface syntaxes:
    /// db.execute_query(&simple, "MATCH NODE WHERE label = \"City\"")?;
    /// db.execute_query(&cypher, "MATCH (n:City) RETURN n")?;
    /// ```
    pub fn execute_query(
        &mut self,
        language: &dyn QueryLanguagePort,
        query: &str,
    ) -> Result<QueryResult, GraphError> {
        language.execute(query, self)
    }

    /// Convenience: load all nodes through the cache/storage stack.
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

    /// Convenience: load all edges through the cache/storage stack.
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
}

// ── DatabaseContext implementation ────────────────────────────────────────────
//
// The query subsystem never imports LayeredGraphDatabase directly.
// It receives a &mut dyn DatabaseContext, which this impl satisfies.
// This keeps the query layer decoupled from the database layer.

impl DatabaseContext for LayeredGraphDatabase {
    fn get_node(&mut self, id: NodeId) -> Result<Option<Node>, GraphError> {
        self.get_node(id)
    }

    fn get_all_nodes(&mut self) -> Result<Vec<Node>, GraphError> {
        self.all_nodes()
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
