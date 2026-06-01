# 10 — Scale & Production Concerns

This document describes the current limitations of AdGraphDb and the
concrete techniques used by production graph databases to address them.

---

## Current limitations

| Limitation | Impact | Solution |
|-----------|--------|---------|
| WAL replay on every scan | O(N) reads for `load_all_nodes` | B-tree / LSM storage |
| No property indexes | O(N) query filter evaluation | Secondary indexes |
| Single-threaded | Only one reader or writer at a time | MVCC / read-write locks |
| All structure in RAM | Graph must fit in adjacency list memory | Disk-resident graph |
| No transactions | No atomic multi-operation changes | MVCC or 2PL |
| No query planning | No cost-based optimisation | Query planner |
| WAL grows unbounded | Must call compact() manually | Auto-compaction |

---

## 1. Storage at scale

### Current: WAL replay

Every call to `load_all_nodes` reads the entire WAL file. For 10 000 nodes
this is fast; for 100 million nodes it takes seconds per read.

### Solution: B-tree page storage

A B-tree organises data into fixed-size pages (typically 4KB or 16KB).
Each page holds multiple records, sorted by key.

```
Root page
  ├─ Page 1 [N0..N999]
  │    ├─ Page 11 [N0..N499]
  │    └─ Page 12 [N500..N999]
  └─ Page 2 [N1000..N9999]
       └─ ...
```

Lookup is O(log N) — traverse the tree to the leaf page.
Insert is O(log N) — find the page, insert, possibly split.

**Implementation sketch:**

```rust
// src/adapters/storage/btree_file.rs
pub struct BTreeFileStorage {
    page_file: File,          // pages stored in a single file
    root_page: u64,           // page number of the root
    free_list: Vec<u64>,      // recycled page numbers
}

impl StoragePort for BTreeFileStorage { ... }
```

### Solution: LSM-tree (Log-Structured Merge Tree)

Used by RocksDB, LevelDB, Cassandra. Writes go to an in-memory buffer
(memtable), which is periodically flushed to sorted immutable files (SSTables).
Reads merge results from all levels.

- Write speed: extremely fast (sequential writes only)
- Read speed: O(log N) with bloom filters
- Compaction: background merge of SSTables

The WAL in AdGraphDb is already LSM-like at the log level —
the full LSM adds the sorted-file layer and automatic background compaction.

---

## 2. Indexing

### Current: full scan

Queries like `MATCH NODE WHERE label = "City"` scan all nodes.
For 1 million nodes with 1000 cities, this reads 999 000 irrelevant nodes.

### Solution: label index

```rust
// A HashMap from label → set of NodeIds
pub struct LabelIndex {
    index: HashMap<String, HashSet<NodeId>>,
}

impl LabelIndex {
    fn add(&mut self, id: NodeId, label: &str) {
        self.index.entry(label.to_string()).or_default().insert(id);
    }
    fn lookup(&self, label: &str) -> impl Iterator<Item = &NodeId> {
        self.index.get(label).into_iter().flatten()
    }
}
```

With a label index, `MATCH (n:City)` becomes O(city_count) instead of O(N).

### Solution: property index (BTree-based range index)

For `WHERE population > 1_000_000`, a sorted index (e.g. `BTreeMap`) allows
binary search to find the first matching node, then scan forward:

```rust
pub struct PropertyIndex {
    // BTreeMap gives sorted order for range queries
    index: BTreeMap<Value, HashSet<NodeId>>,
}
```

This makes range queries O(log N + result_size) instead of O(N).

### Solution: composite index

A composite index covers multiple properties simultaneously, allowing the query
planner to use a single index for multi-field filters.

---

## 3. Concurrency

### Current: single-threaded, no locking

`LayeredGraphDatabase` takes `&mut self` on writes and `&self` on reads
(conceptually). No thread-safety is provided.

### Solution: read-write lock (RwLock)

Allow multiple concurrent readers or one exclusive writer:

```rust
use std::sync::RwLock;

pub struct ConcurrentGraphDatabase {
    inner: RwLock<LayeredGraphDatabase>,
}

impl ConcurrentGraphDatabase {
    pub fn get_node(&self, id: NodeId) -> Result<Option<Node>, GraphError> {
        self.inner.read().unwrap().get_node(id)
        //         ^^^^^ multiple readers allowed simultaneously
    }

    pub fn insert_node(&self, ...) -> Result<NodeId, GraphError> {
        self.inner.write().unwrap().insert_node(...)
        //         ^^^^^ exclusive: blocks all readers and other writers
    }
}
```

**Trade-off**: reads block on writes. For read-heavy workloads this is fine;
for write-heavy workloads it becomes a bottleneck.

### Solution: MVCC (Multi-Version Concurrency Control)

MVCC allows readers and writers to proceed concurrently without blocking
each other, by keeping multiple versions of each record:

```
Timeline:
  T=1: Writer inserts N0 v1 (visible to transactions starting at T≥1)
  T=2: Reader starts (sees N0 v1)
  T=3: Writer inserts N0 v2 (visible to transactions starting at T≥3)
  T=2: Reader still sees N0 v1 (its snapshot is from T=2)
  T=4: Reader commits (reads consistent snapshot as of T=2)
```

Each record has a `created_at` and `deleted_at` transaction ID.
Readers use their transaction's start time to filter versions.

Used by PostgreSQL, CockroachDB, and many others.
Implementation complexity is significant but the pattern is well-understood.

---

## 4. Transactions

### Current: no transaction support

Each operation (insert_node, delete_edge, etc.) is independent.
If two operations must be atomic — either both happen or neither — there is
no mechanism to guarantee this.

### Solution: Begin/Commit/Rollback

```rust
pub trait TransactionalDatabase {
    fn begin(&mut self) -> TransactionId;
    fn commit(&mut self, txn: TransactionId) -> Result<(), GraphError>;
    fn rollback(&mut self, txn: TransactionId);
}
```

A WAL-based implementation:
1. `begin` → start buffering operations in a local transaction log
2. On `commit` → write all buffered records to the WAL atomically
3. On `rollback` → discard the buffer (write nothing)

---

## 5. Query optimisation

### Current: naive execution

The executor runs `QueryCommand` literally:
`MatchNodes(filter)` → `get_all_nodes()` → filter in memory.

### Solution: query planner

A query planner chooses the most efficient execution strategy:

```
Input query:   MATCH (n:City) WHERE n.population > 1_000_000

Option A:  Full scan  → filter by label → filter by population
           Cost: O(N)

Option B:  Label index lookup "City" → filter by population
           Cost: O(city_count)

Option C:  Population range index [1_000_000, ∞) → filter by label "City"
           Cost: O(result_count)

Planner chooses: Option B or C (lowest cost)
```

A cost-based planner estimates the number of rows each plan will touch
using statistics (e.g. number of nodes per label, value distributions).

---

## 6. Distributed scaling

### Partitioning (sharding)

Split the graph across multiple machines. Two strategies:

**Vertex-cut partitioning**: assign each edge to a machine;
nodes appear on multiple machines.
```
Machine 1: edges [E0, E1, E4]
Machine 2: edges [E2, E3, E5]
```

**Edge-cut partitioning**: assign each node to a machine;
edges that cross machines are replicated.
```
Machine 1: nodes [N0, N1, N2]  + ghost N3 for cross-machine edges
Machine 2: nodes [N3, N4, N5]  + ghost N1
```

**Challenge**: many graph algorithms (BFS, Dijkstra) require frequent
access to neighbors. Cross-machine edge traversal is expensive.

### Replication

Keep copies of the graph on multiple machines:
- One **primary** handles writes
- Multiple **replicas** handle reads

Write latency increases (must replicate); read throughput scales linearly.

### Consensus

With multiple primaries (for write scaling), you need a consensus protocol
(Raft, Paxos) to agree on the order of writes across machines.

Used by: CockroachDB (Raft), YugabyteDB (Raft), TiKV (Raft).

---

## 7. Memory-mapped files (mmap)

Instead of `File::read`, map the storage file directly into the process's
virtual address space. The OS handles reading pages on demand.

```rust
use std::fs::File;
// (with memmap2 crate)
let mmap = unsafe { MmapOptions::new().map(&file)? };
// Access bytes directly — OS brings them from disk lazily:
let node_id = u64::from_le_bytes(mmap[offset..offset+8].try_into().unwrap());
```

**Advantages:**
- Zero-copy reads (no `read()` syscall, no intermediate buffer)
- OS manages the page cache (not your LruCache)
- Large files can be accessed without loading entirely into RAM

**Disadvantages:**
- Harder to control caching behaviour
- On 32-bit systems, limited to 4GB address space
- Platform-specific; Windows and Linux behave differently

---

## 8. Practical improvement roadmap

Listed in order of educational value and impact:

1. **Auto-compaction** — compact when WAL exceeds a size threshold
2. **Label index** — simple HashMap, dramatic query speedup
3. **Property index** — BTreeMap for range queries
4. **Read-write lock** — enables concurrent reads
5. **Checksums** — CRC32 per WAL record, detect corruption
6. **B-tree storage** — replace WAL replay with O(log N) lookups
7. **Transactions** — begin/commit/rollback semantics
8. **MVCC** — full reader-writer concurrency
9. **Query planner** — cost-based index selection
10. **Distributed** — partition and replicate across machines

Each step is independent and can be implemented as a new adapter,
keeping the Ports & Adapters architecture intact throughout.
