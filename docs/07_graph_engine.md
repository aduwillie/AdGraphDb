# 07 — Graph Engine

The graph engine is the **in-memory structural index** for the graph.

---

## What the engine does (and doesn't do)

| Responsibility | Engine | Storage | Cache |
|---|---|---|---|
| Which nodes exist | ✓ | ✓ | — |
| Which edges connect them | ✓ | ✓ | — |
| Node property data | ✗ | ✓ | ✓ |
| Edge property data | ✗ | ✓ | ✓ |
| Persists to disk | ✗ | ✓ | ✗ |

The engine holds **only structural data** — IDs and connectivity — so
traversals and neighbor lookups happen entirely in RAM with no disk I/O.

---

## GraphEnginePort

```rust
pub trait GraphEnginePort {
    fn insert_node(&mut self, id: NodeId);
    fn remove_node(&mut self, id: NodeId);

    fn insert_edge(&mut self, edge_id: EdgeId, source: NodeId, target: NodeId, weight: f64);
    fn remove_edge(&mut self, edge_id: EdgeId);

    fn neighbors_outgoing(&self, source: NodeId) -> Vec<Neighbor>;
    fn neighbors_incoming(&self, target: NodeId) -> Vec<Neighbor>;

    fn all_node_ids(&self) -> Vec<NodeId>;
    fn all_edge_ids(&self) -> Vec<EdgeId>;

    fn node_count(&self) -> usize;
    fn edge_count(&self) -> usize;
    fn contains_node(&self, id: NodeId) -> bool;
}

pub struct Neighbor {
    pub node_id: NodeId,
    pub edge_id: EdgeId,
    pub weight:  f64,
}
```

`Neighbor` carries the edge ID so callers can load full edge properties
from the cache/storage if needed.

---

## AdjacencyListEngine

**File:** `src/adapters/engine/adjacency_list.rs`

### The adjacency list representation

For each node, store a **list of its neighbors**:

```
Outgoing (from each node):
  N0: [(N1, E0, 457.0), (N2, E1, 370.0)]
  N1: [(N2, E2, 265.0)]
  N2: []

Incoming (to each node):
  N0: []
  N1: [(N0, E0, 457.0)]
  N2: [(N0, E1, 370.0), (N1, E2, 265.0)]
```

Maintaining **both** directions (outgoing and incoming) means both
`neighbors_outgoing` and `neighbors_incoming` are O(degree) — no scan needed.

### Internal data structure

```rust
struct AdjacencyListEngine {
    outgoing:       HashMap<NodeId, Vec<Neighbor>>,
    incoming:       HashMap<NodeId, Vec<Neighbor>>,
    edge_endpoints: HashMap<EdgeId, (NodeId, NodeId)>,
}
```

`edge_endpoints` allows `remove_edge` to find which nodes' lists to update
in O(1) without scanning all adjacency lists.

### Complexity table

| Operation | Time complexity | Notes |
|-----------|----------------|-------|
| `insert_node` | O(1) | HashMap entry |
| `remove_node` | O(E_v) | Must scan edges; E_v = degree of v |
| `insert_edge` | O(1) | Append to Vec |
| `remove_edge` | O(degree) | Linear scan of Vec to remove entry |
| `neighbors_outgoing` | O(degree) | Slice copy |
| `neighbors_incoming` | O(degree) | Slice copy |
| `contains_node` | O(1) | HashMap lookup |
| `all_node_ids` | O(V) | HashMap key collection |

---

## Why maintain two directions?

Many graph queries need both:
- **Outgoing neighbors**: "Who does Alice follow?" (used by BFS/DFS/Dijkstra)
- **Incoming neighbors**: "Who follows Alice?" (reverse lookup)

Without the `incoming` map, reverse lookups require a full scan of all edges:
O(E) instead of O(degree). For large graphs this is the difference between
milliseconds and seconds.

### Analogy: phone directory

An outgoing-only adjacency list is like a phone book sorted by name.
You can look up Alice's number quickly (outgoing), but to find everyone
who has Alice's number (incoming) you'd have to read the entire book.

The incoming map is like a **reverse phone directory** — you can look up
a number and find the person's name immediately.

---

## Engine lifecycle

```
startup:
  for each node in storage.load_all_nodes():
    engine.insert_node(node.id)

  for each edge in storage.load_all_edges():
    engine.insert_edge(edge.id, edge.source, edge.target, edge.weight)

mutation:
  insert_node → engine.insert_node(id)
  insert_edge → engine.insert_edge(id, src, tgt, weight)
  delete_node → engine.remove_node(id)   [also removes incident edges]
  delete_edge → engine.remove_edge(id)

shutdown:
  (drop — no serialization needed; rebuilt from storage on next open)
```

---

## Adjacency list vs adjacency matrix

### Adjacency matrix

Store a V×V boolean (or weight) matrix where `matrix[i][j]` = edge from i to j.

```
     N0    N1    N2
N0  [  ]  [457] [370]
N1  [  ]  [  ]  [265]
N2  [  ]  [  ]  [  ]
```

| Adjacency list | Adjacency matrix |
|---|---|
| Space: O(V + E) | Space: O(V²) |
| Edge lookup: O(degree) | Edge lookup: O(1) |
| Add node: O(1) | Add node: O(V) — expand matrix |
| Neighbor list: O(degree) | Neighbor list: O(V) — scan row |
| Best for: sparse graphs | Best for: dense graphs |

Most real graphs are sparse (V=1M nodes, E=10M edges → 0.001% density).
An adjacency matrix for 1M nodes would require 1TB of RAM. An adjacency list
needs only the edges that actually exist.

---

## Adding a new engine

The clean separation means you can swap in a different graph representation
without touching any other code:

```rust
// src/adapters/engine/adjacency_matrix.rs
pub struct AdjacencyMatrixEngine {
    // Dense V×V matrix — only practical for small graphs
    matrix: Vec<Vec<Option<(EdgeId, f64)>>>,
    node_to_index: HashMap<NodeId, usize>,
    ...
}

impl GraphEnginePort for AdjacencyMatrixEngine { ... }
```

Pass it to `LayeredGraphDatabase::open`:
```rust
Box::new(AdjacencyMatrixEngine::new())
```

See [09_adding_adapters.md](09_adding_adapters.md) for a complete guide.
