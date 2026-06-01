# 04 — Persistence & Write-Ahead Log

---

## What is persistence?

A database is only useful if data survives a process restart.
"Persistence" means: write data to a durable medium (disk) so it can be
recovered even if the power fails or the process crashes.

### Analogy: a restaurant order book

Imagine a restaurant where waiters shout orders at the kitchen — but what
if the kitchen forgets? The solution is an **order book**: every order is
written down before being prepared. If a waiter drops their notepad, the
kitchen can re-read the book and reconstruct what was ordered.

A **Write-Ahead Log (WAL)** is the database equivalent of the order book.

---

## Write-Ahead Log (WAL)

A WAL is an **append-only file** of mutation records.

Instead of modifying data in-place (dangerous — a crash mid-write leaves
a corrupt file), every mutation **appends** a new record to the end of a log file.

```
Disk file (grows downward):
  ┌─────────────────────────────┐
  │ UpsertNode(N0, "London")    │  ← written first
  │ UpsertEdge(E0, N0→N1)       │
  │ UpsertNode(N1, "Paris")     │
  │ DeleteNode(N0)              │  ← tombstone: N0 is gone
  │ UpsertNode(N0, "New London")│  ← N0 re-created with new data
  └─────────────────────────────┘
```

### Reading (replay)

To reconstruct the current state, replay all records in order.
Later records shadow earlier ones:

```
After replay:
  N1 → "Paris"        (from UpsertNode)
  N0 → "New London"   (last UpsertNode for N0 wins)
  E0 → deleted        (no UpsertEdge visible after DeleteNode cleared N0)
```

---

## Record types

Both adapters (JSON and binary) use the same four logical record types:

| Type | Meaning |
|------|---------|
| `UpsertNode` | Create or overwrite a node |
| `UpsertEdge` | Create or overwrite an edge |
| `DeleteNode` | Tombstone — node is logically deleted |
| `DeleteEdge` | Tombstone — edge is logically deleted |

"Upsert" = insert if new, update if already exists.

---

## The write path in LayeredGraphDatabase

```
insert_node(label, props):
  Step 1 →  storage.save_node(&node)    ← DURABLE FIRST
  Step 2 →  cache.put_node(node)        ← RAM updated
  Step 3 →  engine.insert_node(id)      ← structural index updated
```

**Step 1 must happen before steps 2 and 3.** If the process crashes between
step 1 and step 2, the node is still on disk and will be recovered on the
next `open()`. If step 1 fails, the node is never in the cache or engine.

This guarantees **write durability**: a successful return from `insert_node`
means the data is on disk.

---

## The read path

```
get_node(id):
  Step 1 →  cache.get_node(id)     → if Some, return (RAM hit, no disk)
  Step 2 →  storage.load_node(id)  → replay WAL, find node
  Step 3 →  cache.put_node(node)   → warm the cache for next read
  return node
```

Storage is only hit on a **cache miss**. After the first read, subsequent
reads of the same node are served from RAM.

---

## Database open() — startup sequence

```
LayeredGraphDatabase::open(storage, cache, engine):
  1. storage.load_all_nodes()     ← replay all WAL records
  2. For each node:
       id_gen.seed_from_node(id)  ← ensure new IDs won't collide
       engine.insert_node(id)     ← rebuild adjacency index
  3. storage.load_all_edges()
  4. For each edge:
       id_gen.seed_from_edge(id)
       engine.insert_edge(id, src, tgt, weight)
  5. Cache starts empty (cold)
```

The engine and ID generator are **always derived from storage** — they are
never the source of truth. This means a cold restart recovers completely.

---

## Compaction

Over time a WAL grows because:
- Updates write a new record without removing the old one
- Deletes write a tombstone but the original record remains

```
Before compaction (7 records):    After compaction (2 records):
  UpsertNode(N0, v1)  ←stale      UpsertNode(N0, v2)  ← live
  UpsertNode(N1)                  UpsertNode(N1)       ← live
  UpsertNode(N0, v2)  ← current
  UpsertNode(N2)      ← deleted
  DeleteNode(N2)      ← tombstone
  UpsertNode(N0, v3)  ← wait,
  UpsertNode(N0, v2)  ← actually use this one
```

Compaction rewrites the file with only the current live state.

### Atomic rename

To prevent a corrupt file if the process crashes during compaction:

```
1. Write new state to "graph.json.tmp"
2. fsync (ensure bytes are on disk)
3. Rename "graph.json.tmp" → "graph.json"   ← atomic on POSIX
4. Reopen the write handle on the new file
```

Step 3 is atomic on all major operating systems: readers always see either
the old file or the new file, never a partial mix.

```rust
db.compact()?;  // safe to call at any time
```

After compaction, the cache is cleared (the write pointer moved).

---

## Durability guarantees

| Scenario | Data safe? |
|----------|-----------|
| Process crash after successful `insert_node` | ✓ Yes — on disk from step 1 |
| Process crash mid-compaction | ✓ Yes — .tmp file is discarded, original intact |
| OS crash (power failure) | Depends on OS/hardware flush; call `File::sync_all` for full safety |
| Disk full during write | ✗ Error returned; partial write possible (implement checksums to detect) |

For a production database, add per-record checksums (CRC32) to detect
partial writes after a power failure. See [10_scale_and_production.md](10_scale_and_production.md).

---

## WAL vs B-tree storage

| | WAL (this implementation) | B-tree |
|--|--|--|
| Write speed | O(1) append | O(log N) in-place update |
| Read speed | O(N) replay | O(log N) lookup |
| Space efficiency | Grows with mutations | Proportional to live data |
| Compaction needed | Yes | No (but needs vacuuming) |
| Crash safety | Inherent (append-only) | Requires careful locking |
| Educational clarity | High — linear file, easy to inspect | Medium — complex page management |

WAL is used by PostgreSQL, SQLite (WAL mode), RocksDB, and many others.
See [10_scale_and_production.md](10_scale_and_production.md) for how to evolve toward B-tree storage.
