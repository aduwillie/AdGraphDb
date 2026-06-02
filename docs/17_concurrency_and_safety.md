# 17 — Concurrency, Data Safety, and Crash Recovery

> **What this document covers:**
> How AdGraphDb handles multiple clients, protects data integrity with checksums
> and WAL transaction markers, and what it would take to add full MVCC.

---

## Part 1 — Multi-threaded server

### Current model: Mutex serialisation

`SharedDatabase` in `src/concurrent/mod.rs` wraps `LayeredGraphDatabase` in
`Arc<Mutex<>>`.  The server spawns one OS thread per connection; each thread
calls `db.lock()` to acquire the Mutex before running a query.

```
Thread 1:  lock()  → execute_query(…)  → unlock()
Thread 2:                    wait…      lock()  → execute_query(…)  → unlock()
Thread 3:                               wait…                       wait…
```

**Correctness:** guaranteed — only one thread touches the database at a time.
**Throughput:** limited to one query concurrently.
**Latency:** bounded by the duration of the longest running query.

For most educational and small-production uses this is acceptable.

### Why `LayeredGraphDatabase` can't currently be shared with RwLock

The obstacle is `CachePort::get_node(&mut self)`.  The LRU cache updates
recency on every read — a write to internal state — so read methods take
`&mut self`.  `RwLock::read()` provides only `&self`.

**Solution (next step):** make the cache use interior mutability.

```rust
// Today:
fn get_node(&mut self, id: NodeId) -> Option<Node>   // prevents RwLock readers

// With interior mutability:
fn get_node(&self, id: NodeId) -> Option<Node>       // compatible with RwLock

// Implementation: wrap HashMap in Mutex inside the cache struct:
pub struct ThreadSafeLruCache {
    inner: Mutex<LruCacheInner>,
}
impl CachePort for ThreadSafeLruCache {
    fn get_node(&self, id: NodeId) -> Option<Node> {
        self.inner.lock().unwrap().get(id)
    }
}
```

Once the cache takes `&self`, `LayeredGraphDatabase::get_node` can too, and
then you can use `Arc<RwLock<LayeredGraphDatabase>>`:

```
Thread 1 (read):   rlock()  → get_node(…)             → unlock()
Thread 2 (read):   rlock()  → get_node(…)             → unlock()  // ← concurrent!
Thread 3 (write):  wlock()  → insert_node(…)          → unlock()  // ← exclusive
```

---

## Part 2 — Checksums (data integrity)

### Why checksums matter

A WAL record is a sequence of bytes.  If the disk returns corrupted bytes
(bit rot, bad sector, write-tearing) without a checksum, the database may:
- Load a node with scrambled properties
- Misinterpret a field as a different type
- Crash with an out-of-bounds decode

Checksums detect corruption before it propagates to query results.

### Adler-32 implementation

`BinaryFileStorage` appends a 4-byte Adler-32 checksum to every record.
The checksum covers all bytes in the record including the type tag:

```
Record bytes: [0x01][node payload…][0x00 0x00 0x00 0x00]  ← checksum appended
                                    ↑ adler32([0x01][node payload…])
```

The algorithm (from RFC 1950):

```rust
fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a.wrapping_add(byte as u32)) % MOD;
        b = (b.wrapping_add(a))           % MOD;
    }
    (b << 16) | a
}
```

On **WAL replay**, each record's stored checksum is verified against a
freshly computed one.  A mismatch prints a warning and **skips the record**
rather than crashing — the database stays usable with partial data loss
instead of refusing to open.

### JSON storage

`JsonFileStorage` does not implement checksums (JSON is human-readable and
self-describing; corruption usually produces a parse error, which is
reported per-line during replay).

---

## Part 3 — WAL transaction markers (crash safety)

### The problem without markers

`commit_transaction` writes multiple WAL records.  If the process crashes
mid-write:

```
WAL:  UpsertNode(N5)  UpsertNode(N6)  [CRASH — UpsertEdge(E3) never written]
```

On restart, N5 and N6 exist but E3 does not.  The data is partially applied —
a half-committed state that the application may not expect.

### The solution: BEGIN_TXN / COMMIT_TXN markers

`BinaryFileStorage` supports three new record types:

| Record | Byte | Payload |
|--------|------|---------|
| `BEGIN_TXN` | `0x05` | `u64` transaction ID |
| `COMMIT_TXN` | `0x06` | `u64` transaction ID |
| `ROLLBACK_TXN` | `0x07` | `u64` transaction ID |

`commit_transaction` in `LayeredGraphDatabase` wraps its writes:

```
WAL:  BEGIN_TXN(42)  UpsertNode(N5)  UpsertNode(N6)  UpsertEdge(E3)  COMMIT_TXN(42)
```

### Crash recovery on replay

During `replay()`, `BinaryFileStorage` uses a buffer:

```
Read BEGIN_TXN(42)   → start buffering records tagged with txn_id=42
Read UpsertNode(N5)  → buffer
Read UpsertNode(N6)  → buffer
Read UpsertEdge(E3)  → buffer
Read COMMIT_TXN(42)  → apply entire buffer atomically
```

If the file ends before `COMMIT_TXN`:

```
Read BEGIN_TXN(42)   → start buffering
Read UpsertNode(N5)  → buffer
[EOF]                → discard buffer — transaction was incomplete
```

N5 and N6 never appear in the database.  The database is in a consistent
pre-transaction state.

### What "atomicity" means here

All-or-nothing applies only to the **WAL**.  Once COMMIT_TXN is written,
the data is durable.  The in-memory state (cache, engine, indexes) is
rebuilt from the WAL on the next startup, so they always reflect exactly
what the WAL says.

This is the same model used by SQLite (WAL journal) and PostgreSQL (WAL).

---

## Part 4 — Auto-compaction

A WAL-only database has a space problem: every write appends a new record.
Updates produce duplicate records; deletes produce tombstones.  Both waste
disk space and slow down replay.

### Without compaction

```
WAL after 1000 updates to N0:
  UpsertNode(N0, v1)
  UpsertNode(N0, v2)
  ...
  UpsertNode(N0, v1000)   ← only this one is live
```

Replay time grows with every update, not just with the number of live records.

### With auto-compaction

`DatabaseConfig::auto_compact_after_writes` triggers `compact()` automatically:

```rust
fn on_write(&mut self) {
    self.writes_since_compact += 1;
    if let Some(threshold) = self.config.auto_compact_after_writes {
        if self.writes_since_compact >= threshold {
            self.compact().ok();   // best-effort — errors logged
        }
    }
}
```

`compact()` rewrites the WAL to contain only the live state:

```
Compacted WAL:
  UpsertNode(N0, v1000)   ← only the latest version
```

The operation is atomic: writes go to a `.tmp` file, then the file is
renamed over the original (rename is atomic on most OS/file systems).

---

## Part 5 — Full MVCC (future work)

Multi-Version Concurrency Control allows readers to see a consistent snapshot
without blocking writers, and writers to work concurrently with readers.

### How it would work

Every record gets a **version number** (transaction timestamp).
Readers see all records with `version ≤ read_snapshot`.
Writers append new versions; old versions are garbage-collected asynchronously.

```
Record:  { id: N0, version: 5, label: "City", props: {name: "London"} }
Record:  { id: N0, version: 9, label: "City", props: {name: "London-v2"} }

Reader with snapshot=7:  sees N0 at version 5
Reader with snapshot=10: sees N0 at version 9
Writer at version 11:    appends new record, does not block readers
```

### Implementation sketch

1. Add `version: u64` to `Node` and `Edge`.
2. `IdGenerator` becomes a transaction timestamp generator.
3. `StoragePort::load_node(id, snapshot)` returns the latest version ≤ snapshot.
4. The BTree property index and LabelIndex gain version awareness.
5. A garbage collector periodically removes versions no reader can see.

This is several weeks of work on top of the current architecture.
The current design (Mutex + WAL transactions) is a stepping stone.

---

## Part 6 — Current production readiness summary

| Capability | Status | Notes |
|-----------|--------|-------|
| Multi-threaded server | ✓ | Arc<Mutex> serialisation |
| Data integrity (checksums) | ✓ | Adler-32 on binary WAL |
| Crash-safe transactions | ✓ | BEGIN/COMMIT_TXN markers (binary WAL) |
| Auto-compaction | ✓ | Configurable write threshold |
| Concurrent reads | ✗ | Blocked by &mut self cache; needs interior mutability |
| Full MVCC | ✗ | Future work — see Part 5 |
| Distributed | ✗ | Requires consensus, partitioning — out of scope |
| Point-in-time recovery | ✗ | Would need WAL archiving |
| Online schema changes | ✗ | Labels and property names are unschematised |
