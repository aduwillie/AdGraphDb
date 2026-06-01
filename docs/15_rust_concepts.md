# 15 — Rust Concepts Used in This Repo

Every Rust feature used in AdGraphDb is explained here with the exact lines
in this codebase where it appears and why it was chosen.  Read this alongside
the source code to build an accurate mental model of the language.

---

## 1. The newtype pattern — `NodeId(u64)` and `EdgeId(u64)`

**File:** `src/core/node.rs`, `src/core/edge.rs`

```rust
pub struct NodeId(pub u64);
pub struct EdgeId(pub u64);
```

A newtype is a struct with exactly one field.  It creates a new, distinct
type from an existing one.

**Why not just use `u64`?**

```rust
// Without newtypes:
fn get_edge(id: u64) -> Option<Edge> { ... }

get_edge(some_node_id);  // compiles silently — BUG
get_edge(42);            // compiles silently — BUG

// With newtypes:
fn get_edge(id: EdgeId) -> Option<Edge> { ... }

get_edge(NodeId(0));   // compile error: expected EdgeId, found NodeId
get_edge(42);          // compile error: expected EdgeId, found u64
get_edge(EdgeId(42));  // ✓ correct
```

The compiler enforces correct usage.  At runtime, a newtype has exactly the
same memory layout as its inner type — it is a **zero-cost abstraction**.

**Derive macros on newtypes:**
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub u64);
```

- `Copy` — because `u64` is `Copy`, a `NodeId` can be copied by value instead
  of moved.  This means `fn traverse(start: NodeId)` does not consume `start`.
- `Hash` — lets `NodeId` be used as a `HashMap` key.
- `Eq` — required for `HashMap` keys (along with `Hash`).

---

## 2. Traits — defining shared behaviour

**Files:** `src/ports/storage.rs`, `src/ports/cache.rs`, `src/ports/engine.rs`, `src/ports/algorithm.rs`

A **trait** in Rust is what other languages call an interface.  It defines a
set of methods without providing implementations.

```rust
// src/ports/storage.rs
pub trait StoragePort {
    fn save_node(&mut self, node: &Node) -> Result<(), GraphError>;
    fn load_node(&self, id: NodeId) -> Result<Option<Node>, GraphError>;
    fn delete_node(&mut self, id: NodeId) -> Result<(), GraphError>;
    // ...
}
```

Any type that provides these methods "implements" the trait:

```rust
// src/adapters/storage/json_file.rs
impl StoragePort for JsonFileStorage {
    fn save_node(&mut self, node: &Node) -> Result<(), GraphError> {
        self.append_record(&WalRecord::UpsertNode { node: node.clone() })
    }
    // ...
}
```

The database never names `JsonFileStorage` directly — it holds a
`Box<dyn StoragePort>` and calls trait methods.  This is the Ports & Adapters
pattern implemented with Rust traits.

---

## 3. Trait objects — `Box<dyn Trait>` and `&dyn Trait`

**File:** `src/database/layered.rs`

```rust
pub struct LayeredGraphDatabase {
    engine:  Box<dyn GraphEnginePort>,
    cache:   Box<dyn CachePort>,
    storage: Box<dyn StoragePort>,
}
```

A **trait object** (`dyn Trait`) is a pointer to *some type that implements
the trait*, where the exact type is unknown at compile time.  The compiler
inserts a vtable (a table of function pointers) so the right method is called
at runtime.  This is called **dynamic dispatch**.

`Box<dyn Trait>` heap-allocates the value and erases its concrete type:

```rust
// These all have the same type: Box<dyn StoragePort>
let a: Box<dyn StoragePort> = Box::new(JsonFileStorage::open("g.json")?);
let b: Box<dyn StoragePort> = Box::new(BinaryFileStorage::open("g.bin")?);
let c: Box<dyn StoragePort> = Box::new(InMemoryStorage::new());
```

`&dyn Trait` is the same idea but borrowed (no allocation):

```rust
// src/algorithms/bfs.rs
fn traverse(&self, engine: &dyn GraphEnginePort, start: NodeId) -> Vec<NodeId>
//                          ^^^^^^^^^^^^^^^^^^^^
//          The algorithm doesn't know or care which engine it gets.
```

**Dynamic dispatch vs static dispatch:**

```rust
// Static: compiler generates a separate copy per type (faster, larger binary)
fn traverse<E: GraphEnginePort>(engine: &E, start: NodeId) -> Vec<NodeId>

// Dynamic: one copy, vtable call per method (tiny overhead, more flexible)
fn traverse(engine: &dyn GraphEnginePort, start: NodeId) -> Vec<NodeId>
```

AdGraphDb uses dynamic dispatch for the database struct (flexibility matters
more than the tiny overhead) and lets the algorithm traits use dynamic dispatch
via `Box<dyn TraversalAlgorithm>` at call sites.

---

## 4. `#[derive]` — automatic trait implementations

**Used throughout `src/core/`**

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Null,
    Boolean(bool),
    Integer(i64),
    Float(f64),
    Text(String),
}
```

`#[derive]` is a **proc macro** that generates trait implementations at
compile time.  Each trait:

| Trait | What it provides |
|-------|-----------------|
| `Debug` | `{:?}` formatting via `println!("{node:?}")` |
| `Clone` | `.clone()` method — deep copy of the value |
| `Copy` | Implicit bitwise copy (only for simple types like `NodeId`) |
| `PartialEq` | `==` and `!=` operators |
| `Eq` | Strict equality (required for `HashMap` keys alongside `Hash`) |
| `Hash` | Hashing for use in `HashMap`/`HashSet` |
| `Default` | `IdGenerator::default()` / `IdGenerator::new()` both work |
| `Serialize` | `serde_json::to_string(&node)` — JSON encoding |
| `Deserialize` | `serde_json::from_str(json)` — JSON decoding |

You can implement any of these manually when the derived version isn't right.
For example, `f64` does not implement `Eq` because `NaN != NaN`, so
`HeapEntry` in `src/algorithms/dijkstra.rs` implements `Ord` manually using
`total_cmp` (which handles NaN consistently).

---

## 5. `Result<T, E>` — errors without exceptions

**File:** `src/core/error.rs` and everywhere

Rust has no exceptions.  Every function that can fail returns
`Result<SuccessType, ErrorType>`.

```rust
pub fn insert_node(
    &mut self,
    label: impl Into<String>,
    properties: HashMap<String, Value>,
) -> Result<NodeId, GraphError>
//   ^^^^^^ either a NodeId on success, or a GraphError on failure
```

The caller must handle both cases:

```rust
// Option A: propagate with ?
let id = db.insert_node("City", props)?;
//                                    ^
// If Err, return early with that error (converted via From if needed).

// Option B: match explicitly
match db.insert_node("City", props) {
    Ok(id)  => println!("inserted {id}"),
    Err(e)  => println!("failed: {e}"),
}

// Option C: unwrap (panics on error — fine in tests, dangerous in production)
let id = db.insert_node("City", props).unwrap();
```

**The `?` operator** is syntactic sugar for:
```rust
let result = some_fallible_call();
let value = match result {
    Ok(v)  => v,
    Err(e) => return Err(e.into()),  // .into() converts via From
};
```

**`From` conversions enable automatic error wrapping:**

```rust
// src/core/error.rs
impl From<std::io::Error> for GraphError {
    fn from(e: std::io::Error) -> Self {
        GraphError::StorageIo(e.to_string())
    }
}

// Now ? works across error type boundaries:
fn save_node(&mut self, node: &Node) -> Result<(), GraphError> {
    self.writer.write_all(bytes)?;
    //                         ^
    // write_all returns Result<(), std::io::Error>
    // ? converts it to GraphError via the From impl above
    Ok(())
}
```

---

## 6. `Option<T>` — absence without null

**Used throughout the codebase**

```rust
pub fn load_node(&self, id: NodeId) -> Result<Option<Node>, GraphError>
//                                            ^^^^^^^^^^^
//                   Some(node) if found, None if not found
```

`Option<T>` forces the caller to handle the "nothing here" case.
There is no null pointer — a missing value is an explicit `None` variant.

```rust
// Must unwrap before using:
match db.get_node(id)? {
    Some(node) => println!("{node}"),
    None       => println!("not found"),
}

// Short form — do nothing on None:
if let Some(node) = db.get_node(id)? {
    println!("{node}");
}

// Chain operations safely:
let name = db.get_node(id)?
    .as_ref()                          // Option<&Node>
    .and_then(|n| n.properties.get("name"))  // Option<&Value>
    .map(|v| v.to_string());           // Option<String>
```

---

## 7. Ownership and borrowing — why methods take `&mut self`

Rust's ownership system ensures memory safety without a garbage collector.
Every value has exactly one owner.  References borrow the value temporarily.

**`&self`** — shared borrow; multiple readers allowed simultaneously:
```rust
pub fn node_count(&self) -> usize { self.engine.node_count() }
pub fn neighbors_outgoing(&self, id: NodeId) -> Vec<Neighbor> { ... }
```

**`&mut self`** — exclusive borrow; only one writer at a time:
```rust
pub fn insert_node(&mut self, ...) -> Result<NodeId, GraphError> { ... }
pub fn get_node(&mut self, id: NodeId) -> Result<Option<Node>, GraphError> { ... }
//              ^^^^^^^^
// get_node is &mut because the LRU cache updates its recency counter on reads.
// Without mut, the cache's HashMap could not be modified.
```

This is why `CachePort::get_node` takes `&mut self` even though it's
conceptually a "read":

```rust
// src/ports/cache.rs
pub trait CachePort {
    fn get_node(&mut self, id: NodeId) -> Option<Node>;
    //          ^^^^^^^^
    // Reading from an LRU cache mutates the internal generation counter.
    // Rust forces us to be explicit about this.
}
```

**`Box<T>`** — heap allocation.  Used when:
- The size is unknown at compile time (trait objects have no known size)
- You need to transfer ownership across a boundary

```rust
// Box<dyn StoragePort> can hold ANY type implementing StoragePort,
// regardless of its size.  Without Box, the compiler can't determine
// how many bytes to reserve for the field.
engine: Box<dyn GraphEnginePort>,
```

**Clone vs Copy:**
```rust
// Copy: the value is duplicated silently by bit-copy (for primitives and newtypes)
let id: NodeId = NodeId(5);
let id2 = id;   // id is still valid — it was copied, not moved
println!("{id}");  // ✓ — id was not moved

// Clone: explicit deep copy (for strings, Vecs, Nodes)
let node: Node = ...;
let node2 = node.clone();  // explicit copy — allocates new strings/HashMaps
// node is still valid after clone
```

---

## 8. Pattern matching — `match`, `if let`, `let...else`

**Used throughout**

Rust's `match` is exhaustive — the compiler forces you to handle every case:

```rust
// src/query/executor.rs
match command {
    QueryCommand::MatchNodes(filter) => { ... }
    QueryCommand::MatchEdges(filter) => { ... }
    QueryCommand::GetNode(id)        => { ... }
    QueryCommand::GetEdge(id)        => { ... }
    QueryCommand::Traverse { kind, start } => { ... }
    QueryCommand::ShortestPath { start, goal } => { ... }
    QueryCommand::CountNodes => { ... }
    QueryCommand::CountEdges => { ... }
    // Missing any variant is a compile error.
}
```

**`if let`** — match one variant, ignore others:
```rust
if let Some(node) = cache.get_node(id) {
    return Ok(Some(node));
}
// Falls through when None
```

**`let...else`** (Rust 1.65+) — early return on mismatch:
```rust
// src/adapters/engine/adjacency_list.rs
let Some((source, target)) = self.edge_endpoints.remove(&edge_id) else {
    return;  // Edge didn't exist — nothing to do
};
// source and target are in scope here
```

**Destructuring** — pull fields out of structs or enums directly:
```rust
// Destructure a struct
let Node { id, label, properties } = node;

// Destructure in a for loop
for (key, value) in &node.properties {
    println!("{key} = {value}");
}

// Destructure an enum variant with named fields
if let QueryCommand::Traverse { kind, start } = command {
    // kind and start are bound here
}
```

---

## 9. Closures — anonymous functions

**Used with iterator adapters throughout**

```rust
// A closure that borrows `filter` from its surrounding scope:
let matched: Vec<_> = candidates
    .into_iter()
    .filter(|n| filter.matches(n))   // ← closure: takes &Node, returns bool
    .collect();
```

Closure syntax: `|arguments| body` or `|arguments| { multi-line body }`.

**Closures capture their environment:**
```rust
let threshold = 1_000_000_i64;

let big_cities: Vec<_> = nodes
    .into_iter()
    .filter(|n| {
        // `threshold` is captured by reference (immutable borrow)
        n.properties.get("population")
            .and_then(|v| if let Value::Integer(i) = v { Some(*i) } else { None })
            .map(|pop| pop > threshold)
            .unwrap_or(false)
    })
    .collect();
```

**`move` closures** take ownership of captured variables:
```rust
let label = String::from("City");
// `label` is moved into the closure so it can outlive this scope:
let predicate = move |n: &Node| n.label == label;
```

---

## 10. Iterator adapters — lazy transformation pipelines

**Used throughout, especially in `executor.rs` and algorithm files**

Rust iterators are **lazy** — no work is done until the terminal operation
(`.collect()`, `.count()`, `.any()`, etc.).

```rust
let big_cities: Vec<Node> = all_nodes          // Vec<Node>
    .into_iter()                                // IntoIterator → Iterator<Item=Node>
    .filter(|n| n.label == "City")             // Iterator<Item=Node>
    .filter(|n| population(n) > 1_000_000)     // Iterator<Item=Node>
    .collect();                                 // Vec<Node>  ← work happens here
```

**Common adapters used in this repo:**

| Adapter | What it does |
|---------|-------------|
| `.map(f)` | Transform each element |
| `.filter(pred)` | Keep elements where `pred` returns `true` |
| `.flat_map(f)` | Map then flatten — `[[a,b],[c]]` → `[a,b,c]` |
| `.chain(other)` | Append another iterator |
| `.enumerate()` | Yield `(index, value)` pairs |
| `.any(pred)` | Returns `true` if any element matches |
| `.all(pred)` | Returns `true` if all elements match (short-circuits) |
| `.collect()` | Consume the iterator into a collection |
| `.count()` | Count elements |
| `.copied()` | Copy each `&T` into a `T` (for `Copy` types) |
| `.cloned()` | Clone each `&T` into a `T` |
| `.flatten()` | `Iterator<Item=Iterator>` → flat iterator |
| `.filter_map(f)` | Map then discard `None` results |
| `.retain(pred)` | In-place filter on `Vec` (mutates the vector) |

---

## 11. Collections — choosing the right one

**Used throughout `src/adapters/`**

| Type | When to use | Key operations |
|------|------------|----------------|
| `Vec<T>` | Ordered list, index access, push to end | `push`, `pop`, `[i]`, iteration |
| `HashMap<K,V>` | Fast lookup by key, unordered | `insert`, `get`, `remove`, `entry` |
| `BTreeMap<K,V>` | Sorted by key, range queries | `range`, `iter` in order |
| `HashSet<T>` | Fast membership test, no duplicates | `insert`, `contains`, `remove` |
| `VecDeque<T>` | Queue (FIFO) or deque | `push_back`, `pop_front`, `push_front` |
| `BinaryHeap<T>` | Priority queue (max by default) | `push`, `pop` |

**Why BFS uses `VecDeque` instead of `Vec`:**
```rust
// Vec: pop() removes from the end → LIFO (depth-first)
// VecDeque: pop_front() removes from the front → FIFO (breadth-first)

let mut queue: VecDeque<NodeId> = VecDeque::new();
queue.push_back(start);             // enqueue: O(1)
while let Some(node) = queue.pop_front() {  // dequeue: O(1)
    // ...
}
```

`Vec::pop()` would give DFS behaviour (last in, first out).
`VecDeque::pop_front()` gives BFS behaviour (first in, first out).

**The `entry` API — insert-or-update without double lookup:**
```rust
// Without entry (two lookups):
if !map.contains_key(&key) {
    map.insert(key, Vec::new());
}
map.get_mut(&key).unwrap().push(value);

// With entry (one lookup):
map.entry(key).or_default().push(value);
//              ^^^^^^^^^^^
//    Inserts Default::default() (empty Vec) if key is absent,
//    returns mutable reference to the value either way.
```

Used in `AdjacencyListEngine::insert_edge`:
```rust
self.outgoing.entry(source).or_default().push(Neighbor { ... });
```

---

## 12. Zero-sized types — behaviour without state

**Files:** `src/algorithms/bfs.rs`, `src/algorithms/dfs.rs`, `src/algorithms/dijkstra.rs`, `src/adapters/cache/no_cache.rs`

```rust
pub struct BreadthFirstSearch;  // no fields — size = 0 bytes
pub struct NoCache;             // no fields — size = 0 bytes
```

A zero-sized type (ZST) has no state.  It exists only to implement traits.
Passing `&BreadthFirstSearch` to a function has zero overhead — no allocation,
no pointer indirection.

```rust
// Caller:
db.traverse(&BreadthFirstSearch, start);
//           ^^^^^^^^^^^^^^^^^^^
//           A reference to a zero-sized value — essentially just the vtable pointer
```

This pattern is useful when an algorithm has no configuration parameters.
If an algorithm needed state (e.g., `RandomWalk { steps: 10 }`), it would
simply be a struct with fields.

---

## 13. `impl Into<String>` — flexible function arguments

**File:** `src/core/node.rs`, `src/database/layered.rs`

```rust
pub fn insert_node(
    &mut self,
    label: impl Into<String>,   // ← accepts &str, String, or anything convertible
    properties: HashMap<String, Value>,
) -> Result<NodeId, GraphError>
```

`impl Into<String>` means "any type that can be converted into a `String`."
Both `&str` and `String` implement `Into<String>`, so the caller can pass either:

```rust
db.insert_node("City", props)?;                    // &str
db.insert_node(String::from("City"), props)?;      // String
db.insert_node(format!("{}Node", kind), props)?;   // String from format!
```

Inside the function, `.into()` performs the conversion:

```rust
let node = Node { id, label: label.into(), properties };
//                             ^^^^^^^^^^^
//                 Calls Into<String>::into() on whatever type was passed
```

---

## 14. `Reverse<T>` for min-heap — inverting ordering

**File:** `src/algorithms/dijkstra.rs`

`std::collections::BinaryHeap` is a **max-heap**: `pop()` returns the
largest value.  Dijkstra needs a **min-heap**: pop the smallest distance.

```rust
use std::cmp::Reverse;

let mut heap: BinaryHeap<Reverse<HeapEntry>> = BinaryHeap::new();

// Push: wrap in Reverse to invert the ordering
heap.push(Reverse(HeapEntry { distance: 3.0, node_id: NodeId(1) }));
heap.push(Reverse(HeapEntry { distance: 1.0, node_id: NodeId(0) }));

// Pop: Reverse(min_entry) — the smallest distance comes out first
let Reverse(entry) = heap.pop().unwrap();
assert!((entry.distance - 1.0).abs() < f64::EPSILON);
```

`Reverse<T>` wraps a value and flips its `Ord` implementation:
`Reverse(3) < Reverse(1)` even though `3 > 1`.

---

## 15. `serde` — serialization derive macros

**Files:** `src/core/`, `src/adapters/storage/json_file.rs`

`serde` is a serialization framework.  Adding `#[derive(Serialize, Deserialize)]`
to a type gives it automatic JSON encoding and decoding:

```rust
#[derive(Serialize, Deserialize)]
pub struct Node {
    pub id:         NodeId,
    pub label:      String,
    pub properties: HashMap<String, Value>,
}

// Encode to JSON:
let json = serde_json::to_string(&node)?;
// → {"id":{"0":5},"label":"City","properties":{"name":{"Text":"London"}}}

// Decode from JSON:
let node: Node = serde_json::from_str(&json)?;
```

**`#[serde(tag = "op")]`** — enum variants include a type discriminator field:

```rust
#[derive(Serialize, Deserialize)]
#[serde(tag = "op")]
enum WalRecord {
    UpsertNode { node: Node },
    DeleteNode { id: u64 },
}

// UpsertNode serializes as:  {"op":"UpsertNode","node":{...}}
// DeleteNode serializes as:  {"op":"DeleteNode","id":42}
```

The `"op"` field is the tag — the deserializer reads it first to decide
which enum variant to construct.

---

## 16. `#[cfg(test)]` — conditional compilation

**Used at the bottom of every source file**

```rust
#[cfg(test)]
mod tests {
    use super::*;   // import everything from the parent module

    #[test]
    fn some_test() {
        assert_eq!(NodeId(0).to_string(), "N0");
    }
}
```

`#[cfg(test)]` means "only compile this when running `cargo test`."
The test code does not appear in the release binary — no size overhead.

`use super::*` imports all items from the enclosing module, including private
ones.  This lets tests access implementation details without making them public.

---

## 17. Module system — `pub mod`, `use`, visibility

**Files:** `src/lib.rs`, `src/*/mod.rs`

```
crate root (lib.rs)
  pub mod core        ← visible to external crates
    pub mod node      ← also visible externally
      pub struct Node ← visible externally
      fn helper()     ← private to the node module
    mod private_thing ← only visible within `core`
```

```rust
// src/lib.rs — declare top-level modules
pub mod algorithms;
pub mod adapters;
pub mod core;
pub mod database;
pub mod ports;
pub mod query;
pub mod transaction;
```

```rust
// Import from sibling modules using full paths:
use crate::core::node::{Node, NodeId};
use crate::ports::storage::StoragePort;
```

**`pub(crate)`** — visible within this crate but not to external users:
```rust
// src/transaction/mod.rs
pub(crate) fn new(id_gen_snapshot: IdGenerator) -> Self { ... }
//          ^^^^^^^^^^^
// External callers use db.begin_transaction() instead — they never call
// Transaction::new() directly.
```

---

## 18. `AtomicU64` — lock-free counter in tests

**File:** `src/test_helpers.rs`, `tests/integration_test.rs`

```rust
static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_temp_path(suffix: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("adgraphdb_test_{n}_{suffix}"))
}
```

`static` variables live for the entire program duration.
`AtomicU64` allows concurrent access without a mutex — `fetch_add` atomically
increments the counter and returns the old value.

`Ordering::Relaxed` means: no ordering guarantees relative to other memory
operations — just atomicity of the counter itself.  This is safe here because
we only need unique numbers, not ordering between threads.

---

## 19. `Vec::retain` — in-place filtering

**File:** `src/adapters/engine/adjacency_list.rs`

```rust
// Remove all neighbors that use edge_id
if let Some(neighbors) = self.outgoing.get_mut(&source) {
    neighbors.retain(|n| n.edge_id != edge_id);
    //        ^^^^^^
    // Keeps only elements where the predicate returns true.
    // Modifies the Vec in place — O(n).
}
```

`retain` is equivalent to `neighbors = neighbors.into_iter().filter(pred).collect()`,
but without reallocating.

---

## 20. `Display` — custom string representation

**Files:** `src/core/node.rs`, `src/core/edge.rs`, `src/core/value.rs`, `src/query/result.rs`

```rust
impl std::fmt::Display for Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "({} :{})", self.id, self.label)?;
        if !self.properties.is_empty() {
            write!(f, " {{")?;
            for (i, (k, v)) in self.properties.iter().enumerate() {
                if i > 0 { write!(f, ", ")?; }
                write!(f, "{k}: {v}")?;
            }
            write!(f, "}}")?;
        }
        Ok(())
    }
}
```

`Display` enables `println!("{node}")` and `node.to_string()`.
`Debug` (from `#[derive(Debug)]`) enables `println!("{node:?}")`.
They are separate traits because their audiences differ:
- `Display` — for end users (clean, readable output)
- `Debug` — for developers (complete internal representation)

---

## 21. Transaction — patterns used in `src/transaction/mod.rs`

The transaction module uses several Rust patterns together:

**Enum as a tagged union (sum type):**
```rust
pub enum StagedOp {
    InsertNode { node: Node },     // Named fields inside a variant
    InsertEdge { edge: Edge },
    DeleteNode { id: NodeId },
    DeleteEdge { id: EdgeId },
}
```

Each variant can hold different data.  `match` on `StagedOp` forces handling
all four cases — adding a fifth requires updating every match site.

**`pub(crate)` for internal API:**
```rust
pub(crate) fn new(id_gen_snapshot: IdGenerator) -> Self { ... }
pub(crate) fn into_operations(self) -> Vec<StagedOp> { ... }
pub(crate) fn seed_generator_into(self, target: &mut IdGenerator) { ... }
```

These methods are used by `LayeredGraphDatabase` but are not part of the
public API.  External crates cannot call them.

**Consuming `self` (moving out):**
```rust
pub(crate) fn into_operations(self) -> Vec<StagedOp>
//                             ^^^^
// Takes ownership of the Transaction and gives back the operations.
// The Transaction cannot be used after this call — the compiler enforces it.
```

This pattern ("into_*") is idiomatic for conversions that consume the value.

---

## 22. The `Default` trait

**File:** `src/core/id_generator.rs`, `src/adapters/index/label_index.rs`

```rust
#[derive(Debug, Default, Clone)]
pub struct IdGenerator {
    next_node: u64,
    next_edge: u64,
}
```

`#[derive(Default)]` generates `IdGenerator { next_node: 0, next_edge: 0 }` —
zero-initialising all numeric fields.  This lets you write:

```rust
let gen = IdGenerator::default();  // all fields zeroed
// Equivalent to:
let gen = IdGenerator { next_node: 0, next_edge: 0 };
```

`HashMap::entry().or_default()` uses this to insert an empty `Vec` when
the key is absent — `Vec::default()` is an empty `Vec`.

---

## 23. Lifetime-free design

You may notice AdGraphDb has **no lifetime annotations** (`'a`, `'static`)
despite heavy use of references.  This was intentional.

Lifetimes are Rust's compile-time mechanism for tracking how long borrows
are valid.  They become necessary when a struct holds a reference:

```rust
// Requires a lifetime annotation:
struct Ref<'a> {
    data: &'a str,
}
```

AdGraphDb avoids this by:
1. **Cloning values** from the cache instead of returning references
   (`get_node` returns `Option<Node>`, not `Option<&Node>`)
2. **Owning all data** — the database owns the engine, cache, and storage
3. **Returning owned types** — `Vec<NodeId>`, `Vec<Node>`, etc.

The trade-off: cloning costs a small amount of memory and time.
For a database where nodes have string properties, this is negligible.
If it were a performance bottleneck, you could introduce Arc-based
reference counting — but that complexity is not needed here.

---

## Where each concept is used — quick reference

| Concept | Files |
|---------|-------|
| Newtype | `core/node.rs`, `core/edge.rs` |
| Traits | `ports/*.rs` |
| Trait objects `Box<dyn>` | `database/layered.rs` |
| `#[derive]` | All `core/` files |
| `Result<T,E>` + `?` | Every adapter and database method |
| `Option<T>` | `ports/storage.rs`, cache methods, `get_node` |
| `match` / `if let` / `let else` | `query/executor.rs`, `adapters/engine/` |
| Closures | `query/executor.rs`, `algorithms/` |
| Iterator adapters | `query/executor.rs`, `database/layered.rs` |
| `HashMap` / `HashSet` | `adapters/engine/adjacency_list.rs`, `algorithms/` |
| `VecDeque` | `algorithms/bfs.rs` |
| `BinaryHeap` + `Reverse` | `algorithms/dijkstra.rs` |
| Zero-sized types | `algorithms/bfs.rs`, `algorithms/dfs.rs`, `adapters/cache/no_cache.rs` |
| `impl Into<String>` | `database/layered.rs`, `core/node.rs` |
| `serde` derive | `core/` types, `adapters/storage/json_file.rs` |
| `#[cfg(test)]` | Every source file |
| `pub(crate)` | `transaction/mod.rs` |
| `AtomicU64` | `src/test_helpers.rs`, `tests/integration_test.rs` |
| `Vec::retain` | `adapters/engine/adjacency_list.rs` |
| `Display` | `core/node.rs`, `core/edge.rs`, `core/value.rs` |
| `Default` | `core/id_generator.rs`, `adapters/index/label_index.rs` |
| `Clone` (for snapshot) | `core/id_generator.rs` (for Transaction) |
