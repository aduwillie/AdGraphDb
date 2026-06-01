# 06 — Cache Layer

---

## Why cache?

The storage adapters replay the entire WAL file on every scan. For large
graphs this is expensive. The cache intercepts reads and serves hot data
from RAM — orders of magnitude faster than disk.

### Analogy: a browser cache

When you visit a webpage, the browser downloads images and scripts.
If you visit the same page again, the browser serves assets from its local
cache — no network round-trip needed. If the cache is full, it evicts the
oldest (or least-used) items to make room.

AdGraphDb's cache does the same for node and edge property data.

---

## CachePort — the interface

```rust
pub trait CachePort {
    fn get_node(&mut self, id: NodeId) -> Option<Node>;   // &mut for LRU stamp
    fn put_node(&mut self, node: Node);
    fn invalidate_node(&mut self, id: NodeId);

    fn get_edge(&mut self, id: EdgeId) -> Option<Edge>;
    fn put_edge(&mut self, edge: Edge);
    fn invalidate_edge(&mut self, id: EdgeId);

    fn clear(&mut self);
}
```

`get_*` takes `&mut self` because an LRU cache must update recency metadata
on every read — that is a mutation even though the caller just wants to read.

Values are returned by **clone**, not by reference. This avoids borrow
conflicts when the caller wants to subsequently mutate the cache.

---

## LruCache — Least Recently Used eviction

**File:** `src/adapters/cache/lru.rs`

### What is LRU?

LRU evicts the entry that was accessed **least recently** when the cache
is full. The intuition: if an entry hasn't been read in a long time, it's
less likely to be needed again soon.

### Implementation: generation counters

```
A monotonic counter increments on every get or put.
Each entry stores the counter value at its last access.

counter: 7

┌─────┬──────────────┬────────────────────┐
│ Key │  Value       │ Last access (gen)  │
├─────┼──────────────┼────────────────────┤
│ N0  │ Node(London) │     3              │ ← oldest → evict this one
│ N1  │ Node(Paris)  │     6              │
│ N2  │ Node(Berlin) │     7              │ ← most recently used
└─────┴──────────────┴────────────────────┘

Insert N3 when capacity=3:
1. Capacity full — find min generation (N0, gen=3)
2. Evict N0
3. Insert N3 with current counter (gen=8)
```

### Why generation counters vs a doubly-linked list?

| | Generation counters (this impl) | Doubly-linked list (classic LRU) |
|--|--|--|
| get | O(1) update | O(1) update |
| Eviction | O(capacity) scan | O(1) remove head |
| Code lines | ~50 | ~150 |
| Rust borrow complexity | Low | High (self-referential pointers need unsafe) |
| Suitable for | ≤10 000 entries | Any size |

For an educational database with a working set of hundreds of hot nodes,
the generation-counter approach is simpler and correct.

### Separate node and edge capacities

Nodes and edges have independent stores with independent capacities.
This prevents edge-heavy workloads from evicting all node data:

```rust
let cache = LruCache::new(
    512,   // node_capacity — evicts node entries when full
    1024,  // edge_capacity — evicts edge entries when full
);
```

---

## NoCache — pass-through

**File:** `src/adapters/cache/no_cache.rs`

Implements `CachePort` but always returns `None` and discards writes.
Every `get_node` call falls through to storage.

```rust
pub struct NoCache;

impl CachePort for NoCache {
    fn get_node(&mut self, _id: NodeId) -> Option<Node> { None }
    fn put_node(&mut self, _node: Node) {}
    // ...
}
```

**Use cases:**
- **Integration testing** — verify storage correctness without cache interference
- **Benchmarking** — measure raw storage throughput
- **Memory-constrained environments** — trade latency for RAM savings

---

## Cache coherence: preventing stale reads

Stale data occurs when the cache holds an old version of a record that
storage has since updated or deleted.

AdGraphDb prevents this with a simple protocol:

| Event | Cache action |
|-------|-------------|
| `insert_node` | `put_node` (pre-populate with the new node) |
| `get_node` (cache miss) | `put_node` (fill from storage) |
| `delete_node` | `invalidate_node` (remove from cache) |
| `compact()` | `clear()` (file pointer moved; cache is stale) |

Because storage is written **before** the cache is updated on writes, a
cache miss always reads the durable, authoritative value from storage.

---

## Cache architecture in the read path

```
get_node(id):
  ┌──────────────────────────────────────────────────────┐
  │ cache.get_node(id)                                    │
  │   ↓ Some(node)                    ↓ None             │
  │   return node (fast, RAM)         storage.load_node(id) │
  │                                     ↓                │
  │                                   cache.put_node(node) │
  │                                     ↓                │
  │                                   return node (slow, disk) │
  └──────────────────────────────────────────────────────┘
```

First read: slow (disk). Every subsequent read: fast (RAM).

---

## Adding a new eviction policy

1. Create `src/adapters/cache/my_policy.rs`
2. Implement `CachePort`
3. Construct and pass to `LayeredGraphDatabase::open`

### Ideas to explore

**Clock eviction** ("second-chance")
- Uses a circular buffer of entries with a "recently used" bit
- Eviction: scan the clock hand; clear the bit if set, evict if already clear
- O(1) amortised eviction, simpler than doubly-linked LRU

**2Q (Two-Queue)**
- Maintains two queues: a "seen once" FIFO and a "frequently seen" LRU
- Entries graduate from FIFO to LRU on second access
- Better than LRU for scan-heavy workloads (large scans don't pollute the hot set)

**TTL (Time-To-Live)**
- Each entry expires after N seconds regardless of access
- Useful when data changes frequently and staleness is a concern
- Trade-off: adds `Instant` overhead per entry; expired entries linger until eviction

**Size-weighted eviction**
- Assign each entry a "size" (number of property bytes)
- Evict the largest entry when full
- Keeps more entries alive at the cost of larger entries

**Write-through vs write-back**
- Write-through: every write to the cache also writes to storage (current design)
- Write-back: writes go to cache first, flushed to storage later (faster writes, more complex crash recovery)
