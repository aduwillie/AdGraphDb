# 11 — Adding Adapters

Step-by-step code templates for every extension point.

---

## Extension points

| Port trait | What to implement | Plug-in location |
|---|---|---|
| `StoragePort` | Persistence backend | `adapters/storage/` |
| `CachePort` | Eviction policy | `adapters/cache/` |
| `GraphEnginePort` | Graph structure | `adapters/engine/` |
| `TraversalAlgorithm` | Traversal order | `algorithms/` |
| `ShortestPathAlgorithm` | Pathfinding | `algorithms/` |
| `QueryLanguagePort` | Query DSL | `query/languages/` |

All adapters are passed as `Box<dyn Port>` to `LayeredGraphDatabase::open`.
The database has no knowledge of concrete types.

---

## Checklist for every adapter

- [ ] Implement the port trait in full (all methods)
- [ ] Add `pub mod my_adapter;` to the parent `mod.rs`
- [ ] Write `#[cfg(test)]` unit tests in the same file
- [ ] Document trade-offs vs existing adapters in comments
- [ ] No changes needed to `database/`, `algorithms/`, `query/`, or `core/`

---

## Adding a storage backend

**Example: in-memory storage (for testing)**

```rust
// src/adapters/storage/in_memory.rs

use std::collections::HashMap;
use crate::core::{
    edge::{Edge, EdgeId},
    error::GraphError,
    node::{Node, NodeId},
};
use crate::ports::storage::StoragePort;

pub struct InMemoryStorage {
    nodes: HashMap<NodeId, Node>,
    edges: HashMap<EdgeId, Edge>,
}

impl InMemoryStorage {
    pub fn new() -> Self {
        Self { nodes: HashMap::new(), edges: HashMap::new() }
    }
}

impl StoragePort for InMemoryStorage {
    fn save_node(&mut self, node: &Node) -> Result<(), GraphError> {
        self.nodes.insert(node.id, node.clone());
        Ok(())
    }

    fn load_node(&self, id: NodeId) -> Result<Option<Node>, GraphError> {
        Ok(self.nodes.get(&id).cloned())
    }

    fn delete_node(&mut self, id: NodeId) -> Result<(), GraphError> {
        self.nodes.remove(&id);
        Ok(())
    }

    fn save_edge(&mut self, edge: &Edge) -> Result<(), GraphError> {
        self.edges.insert(edge.id, edge.clone());
        Ok(())
    }

    fn load_edge(&self, id: EdgeId) -> Result<Option<Edge>, GraphError> {
        Ok(self.edges.get(&id).cloned())
    }

    fn delete_edge(&mut self, id: EdgeId) -> Result<(), GraphError> {
        self.edges.remove(&id);
        Ok(())
    }

    fn load_all_nodes(&self) -> Result<Vec<Node>, GraphError> {
        Ok(self.nodes.values().cloned().collect())
    }

    fn load_all_edges(&self) -> Result<Vec<Edge>, GraphError> {
        Ok(self.edges.values().cloned().collect())
    }

    fn compact(&mut self) -> Result<(), GraphError> {
        Ok(()) // nothing to compact for in-memory storage
    }
}
```

**Register it:**
```rust
// src/adapters/storage/mod.rs
pub mod binary_file;
pub mod in_memory;   // add this
pub mod json_file;
```

**Use it:**
```rust
let db = LayeredGraphDatabase::open(
    Box::new(InMemoryStorage::new()),
    Box::new(LruCache::new(64, 64)),
    Box::new(AdjacencyListEngine::new()),
)?;
```

---

## Adding a cache eviction policy

**Example: TTL (Time-To-Live) cache**

```rust
// src/adapters/cache/ttl.rs

use std::{collections::HashMap, time::{Duration, Instant}};
use crate::core::{edge::{Edge, EdgeId}, node::{Node, NodeId}};
use crate::ports::cache::CachePort;

struct Entry<V> {
    value:       V,
    inserted_at: Instant,
}

pub struct TtlCache {
    ttl:   Duration,
    nodes: HashMap<NodeId, Entry<Node>>,
    edges: HashMap<EdgeId, Entry<Edge>>,
}

impl TtlCache {
    pub fn new(ttl: Duration) -> Self {
        Self { ttl, nodes: HashMap::new(), edges: HashMap::new() }
    }
    fn expired<V>(&self, e: &Entry<V>) -> bool {
        e.inserted_at.elapsed() >= self.ttl
    }
}

impl CachePort for TtlCache {
    fn get_node(&mut self, id: NodeId) -> Option<Node> {
        match self.nodes.get(&id) {
            Some(e) if !self.expired(e) => Some(e.value.clone()),
            Some(_) => { self.nodes.remove(&id); None }
            None => None,
        }
    }
    fn put_node(&mut self, node: Node) {
        self.nodes.insert(
            node.id,
            Entry { value: node, inserted_at: Instant::now() },
        );
    }
    fn invalidate_node(&mut self, id: NodeId) { self.nodes.remove(&id); }
    fn get_edge(&mut self, id: EdgeId) -> Option<Edge> {
        match self.edges.get(&id) {
            Some(e) if !self.expired(e) => Some(e.value.clone()),
            Some(_) => { self.edges.remove(&id); None }
            None => None,
        }
    }
    fn put_edge(&mut self, edge: Edge) {
        self.edges.insert(
            edge.id,
            Entry { value: edge, inserted_at: Instant::now() },
        );
    }
    fn invalidate_edge(&mut self, id: EdgeId) { self.edges.remove(&id); }
    fn clear(&mut self) { self.nodes.clear(); self.edges.clear(); }
}
```

---

## Adding a graph engine

**Example: adjacency matrix (dense graphs)**

```rust
// src/adapters/engine/adjacency_matrix.rs

use std::collections::HashMap;
use crate::core::{edge::EdgeId, node::NodeId};
use crate::ports::engine::{GraphEnginePort, Neighbor};

/// Space: O(V²). Only practical when V ≤ ~10 000.
pub struct AdjacencyMatrixEngine {
    /// matrix[src_idx][tgt_idx] = Some((EdgeId, weight)) if edge exists
    matrix:         Vec<Vec<Option<(EdgeId, f64)>>>,
    node_to_index:  HashMap<NodeId, usize>,
    index_to_node:  Vec<NodeId>,
}

impl AdjacencyMatrixEngine {
    pub fn new() -> Self {
        Self {
            matrix: Vec::new(),
            node_to_index: HashMap::new(),
            index_to_node: Vec::new(),
        }
    }

    fn grow(&mut self) {
        let new_size = self.index_to_node.len();
        for row in &mut self.matrix {
            row.resize(new_size, None);
        }
        self.matrix.push(vec![None; new_size]);
    }
}

impl GraphEnginePort for AdjacencyMatrixEngine {
    fn insert_node(&mut self, id: NodeId) {
        if self.node_to_index.contains_key(&id) { return; }
        let idx = self.index_to_node.len();
        self.node_to_index.insert(id, idx);
        self.index_to_node.push(id);
        self.grow();
    }

    fn remove_node(&mut self, id: NodeId) {
        if let Some(&idx) = self.node_to_index.get(&id) {
            // Clear row and column (not compacted — educational simplification)
            for col in &mut self.matrix[idx] { *col = None; }
            for row in &mut self.matrix { row[idx] = None; }
        }
    }

    fn insert_edge(&mut self, edge_id: EdgeId, source: NodeId, target: NodeId, weight: f64) {
        if let (Some(&si), Some(&ti)) = (
            self.node_to_index.get(&source),
            self.node_to_index.get(&target),
        ) {
            self.matrix[si][ti] = Some((edge_id, weight));
        }
    }

    fn remove_edge(&mut self, edge_id: EdgeId) {
        for row in &mut self.matrix {
            for cell in row.iter_mut() {
                if cell.map(|(eid, _)| eid) == Some(edge_id) {
                    *cell = None;
                    return;
                }
            }
        }
    }

    fn neighbors_outgoing(&self, source: NodeId) -> Vec<Neighbor> {
        let Some(&si) = self.node_to_index.get(&source) else { return vec![]; };
        self.matrix[si]
            .iter()
            .enumerate()
            .filter_map(|(ti, cell)| {
                cell.map(|(edge_id, weight)| Neighbor {
                    node_id: self.index_to_node[ti],
                    edge_id,
                    weight,
                })
            })
            .collect()
    }

    fn neighbors_incoming(&self, target: NodeId) -> Vec<Neighbor> {
        let Some(&ti) = self.node_to_index.get(&target) else { return vec![]; };
        self.matrix
            .iter()
            .enumerate()
            .filter_map(|(si, row)| {
                row[ti].map(|(edge_id, weight)| Neighbor {
                    node_id: self.index_to_node[si],
                    edge_id,
                    weight,
                })
            })
            .collect()
    }

    fn all_node_ids(&self) -> Vec<NodeId> { self.index_to_node.clone() }

    fn all_edge_ids(&self) -> Vec<EdgeId> {
        self.matrix.iter()
            .flat_map(|row| row.iter())
            .filter_map(|cell| cell.map(|(eid, _)| eid))
            .collect()
    }

    fn node_count(&self) -> usize { self.index_to_node.len() }

    fn edge_count(&self) -> usize {
        self.matrix.iter()
            .flat_map(|row| row.iter())
            .filter(|c| c.is_some())
            .count()
    }

    fn contains_node(&self, id: NodeId) -> bool {
        self.node_to_index.contains_key(&id)
    }
}
```

---

## Adding a traversal algorithm

**Example: bidirectional BFS (faster for finding paths between two nodes)**

```rust
// src/algorithms/bidirectional_bfs.rs

use std::collections::{HashSet, VecDeque, HashMap};
use crate::core::node::NodeId;
use crate::ports::{algorithm::TraversalAlgorithm, engine::GraphEnginePort};

/// Runs BFS from both endpoints simultaneously.
/// Meets in the middle — roughly halves the search space for long paths.
pub struct BidirectionalBfs {
    pub target: NodeId,
}

impl TraversalAlgorithm for BidirectionalBfs {
    fn traverse(&self, engine: &dyn GraphEnginePort, start: NodeId) -> Vec<NodeId> {
        // (simplified: just runs forward BFS — implement meeting-in-middle for real)
        use crate::algorithms::bfs::BreadthFirstSearch;
        use crate::ports::algorithm::TraversalAlgorithm;
        BreadthFirstSearch.traverse(engine, start)
    }
}
```

---

## Adding a query language

**Example: minimal SQL-like language**

```rust
// src/query/languages/sql_lite.rs

pub struct SqlLiteLanguage;

impl QueryLanguagePort for SqlLiteLanguage {
    fn language_name(&self) -> &str { "SQLite" }

    fn execute(&self, query: &str, ctx: &mut dyn DatabaseContext)
        -> Result<QueryResult, GraphError>
    {
        // SELECT * FROM nodes WHERE label = 'City'
        // → QueryCommand::MatchNodes(NodeFilter { label: Some("City"), ... })
        let command = parse_sql(query)?;
        executor::execute(command, ctx)
    }
}

fn parse_sql(q: &str) -> Result<QueryCommand, GraphError> {
    let q = q.trim().to_uppercase();
    if q.starts_with("SELECT * FROM NODES") {
        // Parse WHERE clause if present
        let filter = parse_sql_where(&q)?;
        return Ok(QueryCommand::MatchNodes(filter));
    }
    if q.starts_with("SELECT * FROM EDGES") {
        let filter = parse_sql_edge_where(&q)?;
        return Ok(QueryCommand::MatchEdges(filter));
    }
    if q == "SELECT COUNT(*) FROM NODES" {
        return Ok(QueryCommand::CountNodes);
    }
    Err(GraphError::DeserializationError(format!("[SQLite] unsupported: {q}")))
}
```
