# 14 — Transactions

A transaction is a group of database operations that either all succeed
together or all fail together.  This document explains what that means,
why it matters, how AdGraphDb's implementation works, and what a
fully crash-safe implementation would look like.

---

## Part 1 — Why transactions exist

### The problem: partial writes

Consider registering a new user in a social graph:

```
1. Insert node "Alice" (Person)
2. Insert node "Alice's profile" (Profile)
3. Insert edge Alice → Alice's profile (HAS_PROFILE)
```

What happens if the process crashes after step 1 but before step 3?
Alice exists in the database, but her profile edge does not.
The graph is now **inconsistent** — a Person node with no profile.

A transaction solves this by making the three operations a single unit:
either all three complete, or none of them do.

### The ACID properties

**A**tomicity — all operations in the transaction happen, or none do.
There is no "partially applied" state visible to other readers.

**C**onsistency — the database moves from one valid state to another.
Constraints (e.g. "every Person has a profile edge") hold before and after.

**I**solation — concurrent transactions do not see each other's
in-progress work.  Each transaction appears to execute alone.

**D**urability — once a transaction commits, its changes survive crashes.

AdGraphDb's current implementation provides a subset of these:

| Property | Status | Notes |
|----------|--------|-------|
| Atomicity | Partial | Buffer is atomic pre-commit; crash mid-commit leaves partial writes |
| Consistency | Application responsibility | The DB cannot enforce domain constraints |
| Isolation | None | Single-threaded; no concurrent transactions |
| Durability | Yes (if commit succeeds) | Each op is written to the WAL before the next |

---

## Part 2 — How AdGraphDb transactions work

### The three-phase lifecycle

```
Phase 1 — Begin:    db.begin_transaction()
Phase 2 — Stage:    txn.stage_insert_node(...)
                    txn.stage_insert_edge(...)
                    txn.stage_delete_node(...)
                    txn.stage_delete_edge(...)
Phase 3 — Resolve:  db.commit_transaction(txn)   ← apply to storage/cache/engine
                 OR db.rollback_transaction(txn)  ← discard buffer
```

Nothing touches the database during Phase 2.  All operations are buffered
in the `Transaction` struct's `operations: Vec<StagedOp>` in RAM.

### What `StagedOp` looks like

```rust
// src/transaction/mod.rs
pub enum StagedOp {
    InsertNode { node: Node },
    InsertEdge { edge: Edge },
    DeleteNode { id: NodeId },
    DeleteEdge { id: EdgeId },
}
```

Each variant holds the complete data needed to apply the operation on commit.
The `Node` and `Edge` structs are fully constructed (including IDs) at staging
time — not at commit time.

### How IDs are allocated: the generator snapshot

This is the most important design decision.  Without it, you could not
insert an edge between two nodes in the same transaction, because the nodes
don't have IDs until they are committed.

**The solution:** `begin_transaction()` *clones* the database's `IdGenerator`
and gives the clone to the transaction:

```rust
// src/database/layered.rs
pub fn begin_transaction(&mut self) -> Transaction {
    Transaction::new(self.id_generator.clone())
    //                                 ^^^^^^
    // Clone = independent copy of the counters at this moment
    // The database's own generator is NOT advanced yet
}
```

The transaction allocates IDs from its private copy of the generator.
When you call `txn.stage_insert_node(...)`, the ID is returned immediately:

```rust
// src/transaction/mod.rs
pub fn stage_insert_node(
    &mut self,
    label: impl Into<String>,
    properties: HashMap<String, Value>,
) -> NodeId {
    let id = self.id_gen.next_node_id();  // ← allocates from the snapshot
    self.operations.push(StagedOp::InsertNode {
        node: Node { id, label: label.into(), properties },
    });
    id  // ← returned immediately, usable in subsequent staging calls
}
```

This means you can do:

```rust
let mut txn = db.begin_transaction();

let alice = txn.stage_insert_node("Person", alice_props);  // → NodeId(5)
let bob   = txn.stage_insert_node("Person", bob_props);    // → NodeId(6)

// alice and bob don't exist in the DB yet — but their IDs are known
txn.stage_insert_edge(alice, bob, "KNOWS", 1.0, HashMap::new());

db.commit_transaction(txn)?;  // now all three are written
```

### What commit does (step by step)

```rust
// src/database/layered.rs
pub fn commit_transaction(&mut self, txn: Transaction) -> Result<CommitResult, GraphError> {
    let mut result = CommitResult::default();

    for op in txn.into_operations() {
        match op {
            StagedOp::InsertNode { node } => {
                let id    = node.id;
                let label = node.label.clone();
                self.storage.save_node(&node)?;     // 1. durable first
                self.cache.put_node(node);           // 2. warm cache
                self.engine.insert_node(id);         // 3. structural index
                self.label_index.insert(id, &label); // 4. label index
                self.id_generator.seed_from_node(id);// 5. advance main generator
                result.inserted_node_ids.push(id);
            }
            StagedOp::InsertEdge { edge } => { /* same pattern */ }
            StagedOp::DeleteNode { id }   => { self.delete_node(id)?; }
            StagedOp::DeleteEdge { id }   => { self.delete_edge(id)?; }
        }
    }

    Ok(result)
}
```

Operations are applied **in the order they were staged**.
For each operation: storage is written first (durability), then cache and
engine are updated (consistency with in-memory state).

### What rollback does

```rust
// src/database/layered.rs
pub fn rollback_transaction(&mut self, txn: Transaction) {
    txn.seed_generator_into(&mut self.id_generator);
}

// src/transaction/mod.rs
pub(crate) fn seed_generator_into(self, target: &mut IdGenerator) {
    for op in &self.operations {
        match op {
            StagedOp::InsertNode { node } => target.seed_from_node(node.id),
            StagedOp::InsertEdge { edge } => target.seed_from_edge(edge.id),
            _ => {}
        }
    }
    // `self` is dropped here — Vec<StagedOp> is freed from RAM
    // Nothing was written to storage, cache, or engine
}
```

**Why advance the generator on rollback?**

The transaction allocated IDs N5 and N6 from its snapshot.  After rollback,
those IDs are "unused" — no nodes with those IDs exist in the database.
If the database's generator were not advanced, the next `insert_node` outside
a transaction would also allocate N5, creating a silent ID collision if the
rolled-back data were somehow reused or if logs referenced those IDs.

Advancing past the allocated range guarantees uniqueness even in the
presence of rollbacks.

---

## Part 3 — The commit result

```rust
#[derive(Debug, Default)]
pub struct CommitResult {
    pub inserted_node_ids: Vec<NodeId>,
    pub inserted_edge_ids: Vec<EdgeId>,
    pub deleted_node_ids:  Vec<NodeId>,
    pub deleted_edge_ids:  Vec<EdgeId>,
}
```

`commit_transaction` returns a `CommitResult` listing every ID that was
affected.  This lets callers know which IDs were actually assigned (useful
when the transaction involved multiple inserts):

```rust
let result = db.commit_transaction(txn)?;
println!("Inserted nodes: {:?}", result.inserted_node_ids);
// → Inserted nodes: [N5, N6]
```

---

## Part 4 — Usage patterns

### Pattern 1: Insert interdependent nodes and edges atomically

The classic use case — create a connected subgraph as a unit:

```rust
let mut txn = db.begin_transaction();

let city    = txn.stage_insert_node("City",   props([("name", "Berlin")]));
let country = txn.stage_insert_node("Country", props([("name", "Germany")]));
txn.stage_insert_edge(city, country, "LOCATED_IN", 1.0, HashMap::new());

match db.commit_transaction(txn) {
    Ok(result) => println!("Committed {} nodes", result.inserted_node_ids.len()),
    Err(e)     => println!("Failed: {e}"),  // nothing was written
}
```

### Pattern 2: Conditional write with rollback

Validate before committing; discard on failure:

```rust
let mut txn = db.begin_transaction();
let id = txn.stage_insert_node("City", props);

if some_validation_fails() {
    db.rollback_transaction(txn);  // nothing written; ID not reused
    return Err(GraphError::StorageIo("validation failed".into()));
}

db.commit_transaction(txn)?;
```

### Pattern 3: Bulk import

Load many nodes and edges in one transaction instead of individual inserts:

```rust
let mut txn = db.begin_transaction();

let ids: Vec<NodeId> = cities
    .into_iter()
    .map(|(label, p)| txn.stage_insert_node(label, p))
    .collect();

for (src_idx, tgt_idx, weight) in edges {
    txn.stage_insert_edge(ids[src_idx], ids[tgt_idx], "RAIL", weight, HashMap::new());
}

let result = db.commit_transaction(txn)?;
println!("Imported {} nodes and {} edges",
    result.inserted_node_ids.len(),
    result.inserted_edge_ids.len());
```

### Pattern 4: Atomic delete of a subgraph

Remove a node and all its edges as a unit:

```rust
let mut txn = db.begin_transaction();

// Stage deletion of all outgoing edges first
for neighbor in db.neighbors_outgoing(city_id) {
    txn.stage_delete_edge(neighbor.edge_id);
}
for neighbor in db.neighbors_incoming(city_id) {
    txn.stage_delete_edge(neighbor.edge_id);
}
txn.stage_delete_node(city_id);

db.commit_transaction(txn)?;
```

---

## Part 5 — What the current implementation does NOT provide

### Not crash-safe mid-commit

The current implementation applies operations one at a time on commit.
If the process crashes between operation 2 and operation 3, the WAL will
contain the first two operations but not the third.

On the next startup, those two operations will be replayed — leaving the
database in a partially-committed state with no way to detect this.

**This is the key limitation.**  For many use cases (development, tools,
analytics) it is acceptable.  For production financial or transactional
data it is not.

### No isolation between concurrent users

All operations are single-threaded.  There is no mechanism to prevent
two threads from interleaving their reads and writes.  Transaction A
can see Transaction B's uncommitted data if B commits between A's reads.

### No savepoints

You cannot partially commit or partially rollback.  The entire transaction
commits or the entire transaction is discarded.

---

## Part 6 — Making transactions fully crash-safe

### The WAL transaction marker approach

The WAL already provides a sequential record of all mutations.  Adding
two new record types makes transactions crash-safe:

```
WAL file after a transaction:
  UpsertNode(N0, ...)         ← regular operations before the transaction
  BEGIN_TXN(txn_id=1)         ← marker: transaction started
  UpsertNode(N5, ...)         ← staged ops (now written to WAL at commit time)
  UpsertNode(N6, ...)
  UpsertEdge(E3, ...)
  COMMIT_TXN(txn_id=1)        ← marker: all ops completed successfully
  UpsertNode(N7, ...)         ← regular operations after the transaction
```

**Replay rule**: if the WAL contains a `BEGIN_TXN` without a matching
`COMMIT_TXN`, discard all records between them (they are from a
crashed transaction):

```
Crashed WAL:
  BEGIN_TXN(txn_id=1)
  UpsertNode(N5, ...)   ← discard: no COMMIT seen
  UpsertNode(N6, ...)   ← discard
  [crash here — COMMIT_TXN never written]

On replay: skip N5 and N6 entirely.
```

### Implementation sketch for WAL adapters

```rust
// Add to StoragePort
fn begin_transaction_marker(&mut self, txn_id: u64) -> Result<(), GraphError>;
fn commit_transaction_marker(&mut self, txn_id: u64) -> Result<(), GraphError>;

// Add to WalRecord (JSON adapter)
#[derive(Serialize, Deserialize)]
#[serde(tag = "op")]
enum WalRecord {
    UpsertNode { node: Node },
    UpsertEdge { edge: Edge },
    DeleteNode { id: u64 },
    DeleteEdge { id: u64 },
    BeginTxn   { txn_id: u64 },   // ← new
    CommitTxn  { txn_id: u64 },   // ← new
}

// In replay():
// Track open transaction IDs.
// When a CommitTxn is seen, mark that txn_id as committed.
// After full replay, discard all ops from non-committed txn_ids.
```

### Implementation sketch for LayeredGraphDatabase::commit_transaction

```rust
pub fn commit_transaction(&mut self, txn: Transaction) -> Result<CommitResult, GraphError> {
    let txn_id = self.next_txn_id;
    self.next_txn_id += 1;

    // Write the BEGIN marker first.
    self.storage.begin_transaction_marker(txn_id)?;

    // Write all operations to the WAL.
    for op in txn.operations() {
        match &op {
            StagedOp::InsertNode { node } => self.storage.save_node(node)?,
            StagedOp::InsertEdge { edge } => self.storage.save_edge(edge)?,
            StagedOp::DeleteNode { id }   => self.storage.delete_node(*id)?,
            StagedOp::DeleteEdge { id }   => self.storage.delete_edge(*id)?,
        }
    }

    // Write the COMMIT marker.
    // If the process crashes before this line, the BEGIN is in the WAL
    // but no COMMIT — replay will discard all ops in this transaction.
    self.storage.commit_transaction_marker(txn_id)?;

    // Now update in-memory state (cache, engine, label index).
    // If this crashes, the WAL has the committed data — it can be replayed.
    for op in txn.into_operations() {
        match op {
            StagedOp::InsertNode { node } => {
                self.cache.put_node(node.clone());
                self.engine.insert_node(node.id);
                self.label_index.insert(node.id, &node.label);
            }
            // ...
        }
    }

    Ok(CommitResult { ... })
}
```

With this change, commit is crash-safe:
- Crash before `COMMIT_TXN` → replay discards the partial transaction
- Crash after `COMMIT_TXN` → replay applies all ops; in-memory rebuild
  catches up from the WAL (as it already does on startup)

---

## Part 7 — Isolation: what concurrent access would require

AdGraphDb is single-threaded today.  For concurrent access, two
strategies are standard:

### Read-Write Lock (simple)

```rust
use std::sync::RwLock;

pub struct SharedDatabase {
    inner: RwLock<LayeredGraphDatabase>,
}

impl SharedDatabase {
    // Multiple threads can read simultaneously
    pub fn get_node(&self, id: NodeId) -> Result<Option<Node>, GraphError> {
        self.inner.write().unwrap().get_node(id)
        // (write lock because get_node mutates the LRU cache)
    }

    // Only one thread can write; all readers block
    pub fn begin_transaction(&self) -> MutexGuard<LayeredGraphDatabase> {
        self.inner.write().unwrap()
        // Caller holds the lock for the entire transaction duration
    }
}
```

**Trade-off**: simple, but transactions block all readers.
Acceptable for write-light workloads.

### MVCC (Multi-Version Concurrency Control)

Each write creates a new version of the record tagged with the transaction ID.
Readers see the latest version that was committed *before their transaction started*.

```
Versions of node N0:
  { txn_id: 1, data: "London",     committed: true  }   ← version 1
  { txn_id: 5, data: "London UK",  committed: false }   ← version 2 (in flight)

Reader with start_txn_id = 3:
  → Sees version 1 (txn_id 1 < 3, committed)
  → Does NOT see version 2 (txn_id 5 > 3, or not committed)
```

Readers never block writers; writers never block readers.
Used by PostgreSQL, CockroachDB, and most modern databases.
Implementation complexity is significantly higher than a read-write lock.

---

## Summary

| Question | Answer |
|----------|--------|
| Where is the code? | `src/transaction/mod.rs` |
| Where is it called? | `src/database/layered.rs` (commit, rollback) |
| How are IDs allocated? | IdGenerator cloned at begin; staging allocates from the clone |
| What does commit do? | Applies each StagedOp to storage → cache → engine → label index |
| What does rollback do? | Discards the buffer; advances generator to prevent ID reuse |
| Is it crash-safe? | Not mid-commit; see Part 6 for the WAL marker fix |
| Is there isolation? | No — single-threaded; see Part 7 for concurrent approaches |
| Where is this discussed in other docs? | [10_scale_and_production.md §4](10_scale_and_production.md) (concept overview), [15_rust_concepts.md §21](15_rust_concepts.md) (Rust patterns used) |
