# 13 — Big-O Notation, Scale, and Startup

> This document answers four questions directly:
> 1. What is V (and E, and degree) in Big-O notation?
> 2. What happens if you have a billion nodes and edges?
> 3. Do you have to scan every node to run a filter query?
> 4. How does the graph structure get persisted to disk and rebuilt on startup?

---

## Part 1 — What V, E, and degree mean

Big-O notation describes how an algorithm's cost *grows* as the input grows.
The letters are just short names for quantities.

### V — number of vertices (nodes)

V is the total number of nodes in the graph.

```
Graph with 5 nodes:   V = 5
Graph with 1 million: V = 1_000_000
Graph with 1 billion: V = 1_000_000_000
```

**O(V)** means: if you double the number of nodes, the work doubles.
Example: scanning every node to check its label is O(V).

### E — number of edges

E is the total number of edges in the graph.

```
If each of 5 nodes connects to 3 others:  E ≈ 15
If 1 million nodes each have 10 edges:    E ≈ 10_000_000
```

**O(E)** means: cost scales with the number of edges.
Example: scanning every edge to check its weight is O(E).

### O(V + E) — the cost of most traversals

BFS and DFS are **O(V + E)**.  This means each node is visited at most
once (V visits) and each edge is examined at most once (E examinations).

```
V = 1_000 nodes,  E = 5_000 edges   →   6_000 operations
V = 1_000_000,    E = 5_000_000     →   6_000_000 operations
```

For a sparse graph (E ≈ k×V for small constant k), O(V + E) ≈ O(V).
For a dense graph (every node connects to every other), E ≈ V², so
O(V + E) ≈ O(V²).

### Degree — the number of edges on one node

The **degree** of a single node is how many edges it has.

```
Node "London" connects to Paris, Brussels, Amsterdam → degree = 3
Node "Alice"  has 500 followers and follows 200      → degree = 700
```

**O(degree)** means: cost depends only on *this node's* connections,
not on how large the overall graph is.

`engine.neighbors_outgoing(N0)` is **O(1)** to call and O(degree) to
return — it is a single HashMap lookup returning the pre-built neighbor list.

### Why degree matters so much

```
Graph size    V = 1 billion nodes
Alice's degree    = 150 friends

"Who are Alice's friends?"
  SQL (join):     O(log V)  ≈  30 comparisons   (B-tree index scan on friend table)
  Graph engine:   O(1)      =   1 HashMap lookup returning 150 neighbors

"Friends of friends of Alice?"
  SQL (2 joins):  O(degree × log V)  ≈  150 × 30 = 4_500 index scans
  Graph engine:   O(degree²)         ≈  150 × 150 = 22_500 neighbor lookups
  (all in RAM — no index, no disk)

"Friends 5 hops away?"
  SQL (5 joins):  O(degree⁵ × log V) — hundreds of millions of index scans
  Graph engine:   O(degree⁵)         — same count but each is O(1) RAM access
```

The graph database eliminates the `log V` factor at every hop.
At 5 hops with degree 150 and V = 1 billion, that is `log₂(10⁹) ≈ 30`
— the graph database does 30× fewer operations *per hop* and the advantage
compounds with every additional hop.

---

## Part 2 — What happens with a billion nodes and edges

### The current design is not built for a billion nodes

Let's be concrete about what breaks and what survives.

#### What survives at scale: point lookups and traversals

```
GET NODE N500000000        ← cache hit: O(1) regardless of graph size
TRAVERSE BFS FROM N0       ← O(reachable_V + reachable_E), entirely in RAM
PATH FROM N0 TO N999       ← O((reachable_V + reachable_E) log reachable_V)
```

If the cache is warm, a point lookup is O(1) whether the graph has 100
nodes or 100 billion.  A traversal is bounded by *reachable* nodes from
the starting point — if you start from Alice and she has 10,000 connections
across 5 hops, BFS visits 10,000 nodes regardless of V = 1 billion.

#### What breaks at scale: startup and full scans

```
Problem 1: startup WAL replay
  storage.load_all_nodes()   ← reads EVERY record in the WAL file
  storage.load_all_edges()   ← reads EVERY record in the WAL file
  For 1 billion nodes + 5 billion edges:
    File size:   ~500 GB (binary WAL) to ~2 TB (JSON WAL)
    Time:        hours to days at 500 MB/s disk throughput
    RAM for engine: ~80 bytes × 1B nodes ≈ 80 GB just for outgoing HashMap entries
                   + ~80 bytes × 5B edges ≈ 400 GB for Neighbor structs
    Total RAM:   ~500 GB — exceeds typical server memory

Problem 2: full node/edge scan
  ctx.get_all_nodes()        ← loads all 1 billion nodes
  MATCH NODE WHERE label = "City"
    → examines 1,000,000,000 nodes one by one
    → if 0.01% are cities: 1,000,000 kept, 999,000,000 discarded
    → this is a catastrophic query for any large graph
```

#### The memory breakdown for 1 billion nodes

```rust
// AdjacencyListEngine internal state
outgoing:       HashMap<NodeId, Vec<Neighbor>>
// NodeId = 8 bytes
// Vec<Neighbor>: 24 bytes (ptr + len + cap) + each Neighbor = 24 bytes (NodeId+EdgeId+f64)
// For 1B nodes, average degree 5:
//   HashMap overhead:   ~40 bytes × 1B = 40 GB
//   NodeId keys:         8 bytes × 1B =  8 GB
//   Vec headers:        24 bytes × 1B = 24 GB
//   Neighbor data:      24 bytes × 5B = 120 GB
// outgoing subtotal:  ≈ 192 GB

incoming:       // same size ≈ 192 GB

edge_endpoints: HashMap<EdgeId, (NodeId, NodeId)>
//   40 + 8 + 16 = 64 bytes × 5B edges ≈ 320 GB

Total engine RAM for 1B nodes, 5B edges: ≈ 700 GB
```

A standard server has 64–512 GB of RAM.  700 GB does not fit.

### The three realistic scale regimes

| Graph size | Startup feasible? | Engine fits in RAM? | Full scan speed |
|-----------|-------------------|---------------------|-----------------|
| < 10M nodes | Yes (seconds) | Yes (< 10 GB) | Fast (< 1 s) |
| 10M–100M nodes | Slow (minutes) | Borderline (10–100 GB) | Slow (seconds–minutes) |
| > 100M nodes | No (hours/days) | No | Unusable |

AdGraphDb is designed to demonstrate concepts clearly in the first regime.
The scale problems in regimes two and three are real, and solving them is
what separates educational databases from production systems like Neo4j,
Amazon Neptune, and TigerGraph.

---

## Part 3 — Do you have to scan every node for filter queries?

### In the current implementation: yes

```rust
// query/executor.rs
QueryCommand::MatchNodes(filter) => {
    let all = ctx.get_all_nodes()?;                        // ← ALL nodes loaded
    let matched = all.into_iter()
        .filter(|n| filter.matches(n))                     // ← every node checked
        .collect();
    Ok(QueryResult::Nodes(matched))
}
```

`ctx.get_all_nodes()` calls `engine.all_node_ids()` which returns every
node ID, then loads each node's properties from the cache or storage.
Every node is examined, even those that obviously don't match.

This is the "full table scan" equivalent in a graph database.

### Why this design was chosen for education

The current design is deliberately simple.  The filter sits inside the
executor, after retrieval.  This makes the code easy to follow:

```
retrieve all → filter all → return matched
```

A production database inverts this: **filter during retrieval** using
secondary indexes, so non-matching nodes are never loaded at all.

### The solution: secondary indexes

A **secondary index** maps a property value directly to the set of node
IDs that have it, without touching any other node.

**Label index** (HashMap):
```rust
label_index: HashMap<String, HashSet<NodeId>>

// "City" → { N0, N1, N7, N12, N45, ... }
// "Person" → { N2, N3, N4, N8, ... }
```

Query `MATCH NODE WHERE label = "City"`:

```
Without index (current):    Load all N nodes, check each label.     O(N)
With label index:           label_index.get("City") → set of IDs.  O(1)
```

**Property index** (BTreeMap for range queries):
```rust
property_index: HashMap<String, BTreeMap<Value, HashSet<NodeId>>>
//              ^field name    ^sorted by value

// "population" → BTreeMap {
//     Integer(17_000)      → { N2 }
//     Integer(1_200_000)   → { N3 }
//     Integer(2_100_000)   → { N1 }
//     Integer(9_000_000)   → { N0 }
// }
```

Query `MATCH NODE WHERE props.population > 1_000_000`:

```
Without index:    Load all N nodes, check population.              O(N)
With property index:
  btree.range(Integer(1_000_000)..)  ← binary search to first match  O(log N)
  iterate forward while matches                                       O(results)
  Total:                                                              O(log N + results)
```

**The index lookup cost at different scales:**

| N (total nodes) | Cities (1%) | Full scan | Label index | Property index |
|----------------|-------------|-----------|-------------|----------------|
| 1,000 | 10 | 1,000 ops | 1 op | ~10 ops |
| 1,000,000 | 10,000 | 1,000,000 ops | 1 op | ~17 ops |
| 1,000,000,000 | 10,000,000 | 1,000,000,000 ops | 1 op | ~30 ops |

With a label index, finding all cities in a graph with 1 billion nodes
costs **exactly 1 operation** regardless of graph size.

### How to add indexes to AdGraphDb

The indexes sit alongside the engine.  They are updated on every
`insert_node`, `delete_node`:

```rust
// Sketch: src/adapters/index/label_index.rs
pub struct LabelIndex {
    index: HashMap<String, HashSet<NodeId>>,
}

impl LabelIndex {
    pub fn add(&mut self, id: NodeId, label: &str) {
        self.index.entry(label.into()).or_default().insert(id);
    }
    pub fn remove(&mut self, id: NodeId, label: &str) {
        if let Some(set) = self.index.get_mut(label) {
            set.remove(&id);
        }
    }
    pub fn lookup(&self, label: &str) -> impl Iterator<Item = &NodeId> {
        self.index.get(label).into_iter().flatten()
    }
}
```

The executor would check the index first:
```rust
QueryCommand::MatchNodes(filter) => {
    let candidate_ids = if let Some(label) = &filter.label {
        // Use index: O(1) to find label set
        label_index.lookup(label).copied().collect()
    } else {
        // No label filter: fall back to full scan
        engine.all_node_ids()
    };
    // Load only candidate nodes (much smaller set)
    // Then apply remaining property conditions
}
```

---

## Part 4 — How graph structure is persisted and rebuilt on startup

This is one of the most important things to understand about AdGraphDb's
design.  **The adjacency structure is not stored separately.**

### What "structure" means

The engine holds structural knowledge:
- Which node IDs exist
- For each node: who are its outgoing neighbors (with edge ID and weight)
- For each node: who are its incoming neighbors

```rust
// AdjacencyListEngine
outgoing:       HashMap<NodeId, Vec<Neighbor>>
incoming:       HashMap<NodeId, Vec<Neighbor>>
edge_endpoints: HashMap<EdgeId, (NodeId, NodeId)>
```

### Where structure comes from on disk

There is no separate "adjacency file."  The structure is implicit in the
edge records stored in the WAL:

```
WAL file (binary or JSON):
  UpsertNode(id=N0, label="City", properties={name: "London"})
  UpsertNode(id=N1, label="City", properties={name: "Paris"})
  UpsertEdge(id=E0, source=N0, target=N1, label="RAIL", weight=457.0, properties={})
             ^^^^^^^^^^^^^^^^^^^^^^^^
             source + target ARE the structure
```

Every edge record contains `source` and `target` node IDs.  The adjacency
list is simply these pairs, indexed by source (for outgoing) and by target
(for incoming).

### The full startup sequence, step by step

```
LayeredGraphDatabase::open(storage, cache, engine)
```

**Step 1: Replay the WAL for nodes**

```rust
for node in storage.load_all_nodes()? {
    // storage.load_all_nodes() replays the entire WAL,
    // applying upserts and tombstones in order,
    // returning only the live nodes.

    id_gen.seed_from_node(node.id);
    //  ↑ ensures new IDs never collide with existing ones

    engine.insert_node(node.id);
    //  ↑ registers NodeId(N) in the adjacency HashMap
    //    (no properties stored in engine — only the ID)
}
```

After this step the engine knows: "nodes N0, N1, N2, N3 exist."
It does not know their labels or properties — those live in storage/cache.

**Step 2: Replay the WAL for edges**

```rust
for edge in storage.load_all_edges()? {
    id_gen.seed_from_edge(edge.id);

    engine.insert_edge(edge.id, edge.source, edge.target, edge.weight);
    //  ↑ this one call does three things inside AdjacencyListEngine:
    //
    //  edge_endpoints.insert(E0, (N0, N1))
    //                         ↑ for fast remove_edge
    //
    //  outgoing[N0].push(Neighbor { node_id: N1, edge_id: E0, weight: 457.0 })
    //                     ↑ N0's outgoing neighbor list
    //
    //  incoming[N1].push(Neighbor { node_id: N0, edge_id: E0, weight: 457.0 })
    //                     ↑ N1's incoming neighbor list
}
```

After this step, BFS, DFS, and Dijkstra can all run — they only need
the engine.  Property data is loaded lazily on demand.

**Step 3: Cache starts cold**

No properties are loaded into the cache at startup.  The first time
a node's properties are needed (via `get_node(id)`), the storage layer
is consulted, the node is returned, and it is placed in the cache.

**What is stored where after startup:**

```
┌────────────────────────────────────────────────────────────┐
│  Engine (RAM)                                              │
│  outgoing: { N0: [→N1 via E0 (457km), →N2 via E1 (370km)] │
│              N1: [→N2 via E2 (265km)]                      │
│              N2: []  }                                     │
│  incoming: { N0: []                                        │
│              N1: [←N0 via E0 (457km)]                      │
│              N2: [←N0 via E1, ←N1 via E2] }               │
│  (no labels, no properties — pure structure + weights)     │
├────────────────────────────────────────────────────────────┤
│  Cache (RAM)                                               │
│  (empty on startup — populated lazily)                     │
├────────────────────────────────────────────────────────────┤
│  Storage (disk)                                            │
│  Full WAL: all UpsertNode + UpsertEdge records             │
│  (labels, all properties, structure — everything)          │
└────────────────────────────────────────────────────────────┘
```

### What this means practically

| Action | Where data comes from |
|--------|-----------------------|
| `neighbors_outgoing(N0)` | Engine (RAM) — instant |
| `TRAVERSE BFS FROM N0` | Engine only — instant |
| `PATH FROM N0 TO N5` | Engine only — instant |
| `get_node(N0)` (first call) | Cache miss → Storage replay |
| `get_node(N0)` (later) | Cache hit — instant |
| `MATCH NODE WHERE label = "City"` | Engine gives IDs; Storage gives properties; filter in executor |
| Restart the database | Engine rebuilt from WAL — O(N+E) replay time |

### Startup cost for different graph sizes

```
Startup = storage.load_all_nodes() + storage.load_all_edges()
        = full WAL replay twice
        = O(WAL_size)

WAL size grows with:
  - Number of live records (proportional to N + E)
  - Number of historical updates (until compaction)
  - Record size (~50–200 bytes per node/edge)

Examples:
  100,000 nodes + 500,000 edges
    JSON WAL:   ~100 MB   →  replay < 1 second
    Binary WAL: ~50 MB    →  replay < 0.5 seconds

  1,000,000 nodes + 5,000,000 edges
    JSON WAL:   ~1 GB     →  replay ~2–5 seconds
    Binary WAL: ~500 MB   →  replay ~1–2 seconds

  1,000,000,000 nodes + 5,000,000,000 edges
    JSON WAL:   ~1 TB     →  replay ~30 minutes (at 500 MB/s)
    Binary WAL: ~500 GB   →  replay ~15 minutes
    RAM needed: ~700 GB   →  does not fit in typical server
```

### Why this design was chosen

For an educational database with thousands to millions of nodes:

1. **Simple**: one source of truth (the WAL). No separate "structure file"
   to sync, no two files that can get out of step.
2. **Correct**: if the WAL is intact, the engine can always be rebuilt exactly.
   No possibility of the engine being stale.
3. **Inspectable**: you can read the WAL in a text editor (JSON format)
   and see every edge's source and target.

For production scale, this approach needs two changes:

---

## Part 5 — Production solutions for the scale problems

### Solution A: Checkpoint file for fast startup

Instead of replaying the full WAL on every startup, periodically write
the engine state to a compact binary checkpoint:

```
Startup with checkpoint:
  1. Load checkpoint file (compact binary, only live data)
     → rebuild engine in seconds instead of minutes
  2. Replay only the WAL records AFTER the checkpoint
     → catch up on recent mutations
```

The checkpoint captures exactly what the engine knows:

```
Checkpoint file format:
  Header: [magic] [version] [timestamp]
  Nodes:  [count] [N0] [N1] [N2] ...
  Edges:  [count] [E0: src=N0, tgt=N1, weight=457.0] ...
```

Reading 1 billion nodes from a compact binary checkpoint would be:
- File size: ~8 bytes × 1B = 8 GB (just IDs)
- Time: ~16 seconds at 500 MB/s
- No JSON parsing, no HashMap building of full node records

The engine rebuild from a checkpoint is then O(N + E) but with minimal
constant overhead per record.

### Solution B: Disk-resident engine (B-tree pages)

For graphs that do not fit in RAM, keep the adjacency structure on disk:

```
Page-based adjacency file:
  Page 0: [N0: neighbors=[N1, N2, N5]] [N1: neighbors=[N3, N7]] ...
  Page 1: [N1000: neighbors=[...]] ...

Lookup: neighbors_outgoing(N500000)
  → page_for(N500000)             O(log V) — B-tree index into page file
  → read one page from disk       O(1) disk seek
  → return neighbor list          O(degree)
```

This trades RAM for disk I/O.  Reads are slower than a HashMap lookup
but the graph can be arbitrarily large.

### Solution C: Indexes to eliminate full scans

As described in Part 3, label and property indexes make filter queries
O(1) or O(log N) instead of O(N).

For a billion-node graph, these are not optional — they are mandatory.

### Solution D: Lazy engine population

Instead of loading all N nodes into the engine at startup, load only the
subgraphs that are actually accessed:

```
Startup:         Engine is empty.
First query:     TRAVERSE BFS FROM N5
  → engine does not contain N5
  → load N5's neighbors from storage
  → load their neighbors
  → build engine lazily as the traversal proceeds
```

This makes startup O(1) and shifts the cost to first-access per subgraph.
The trade-off: the first traversal in a cold engine is slower; subsequent
traversals of the same subgraph benefit from the cached structure.

---

## Summary answers to the four questions

### 1. What is V?

V = the total number of vertices (nodes) in the graph.
E = the total number of edges.
Degree = the number of edges on one specific node.

O(V) means cost grows linearly with total nodes.
O(degree) means cost depends only on one node's connections, not the total graph size.

### 2. What if you have a billion nodes?

| Operation | Works at 1 billion? | Why |
|-----------|---------------------|-----|
| `GET NODE Nx` | ✓ Yes | O(1) cache lookup |
| `TRAVERSE BFS FROM Nx` | ✓ Partial | O(reachable) — only visits nodes reachable from start |
| `PATH FROM Nx TO Ny` | ✓ Partial | Same — bounded by reachable subgraph |
| `MATCH NODE WHERE label = "City"` | ✗ No | Full scan — O(1 billion) |
| Startup WAL replay | ✗ No | Reads terabytes, builds 700 GB engine |

### 3. Do you have to scan every node for filter queries?

**In AdGraphDb today: yes.**
The executor calls `ctx.get_all_nodes()` then filters in memory.

**In a production graph database: no.**
Secondary indexes (label index, property indexes) let the query planner
jump directly to matching nodes — O(1) for label, O(log N) for properties.
Adding these to AdGraphDb is the most impactful single improvement.

### 4. How does graph structure persist and load?

**Structure is not stored separately.**
The WAL stores full edge records including `source` and `target` node IDs.
The adjacency lists are *derived* from these on every startup by replaying
the WAL and calling `engine.insert_edge(id, source, target, weight)` for
each live edge.

The engine is transient (RAM only).  The WAL is durable (disk).
Restart = WAL replay = engine rebuild = startup cost proportional to N + E.

For scale: add a checkpoint file that captures the engine state compactly,
so startup replays only recent WAL records instead of the full history.
