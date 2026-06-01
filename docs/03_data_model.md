# 03 — Data Model

AdGraphDb implements the **property graph** model.

---

## NodeId and EdgeId — newtype wrappers

```rust
pub struct NodeId(pub u64);
pub struct EdgeId(pub u64);
```

**Why newtypes instead of raw `u64`?**

A raw `u64` lets you accidentally pass a node ID where an edge ID is expected,
or mix IDs with other numbers. The compiler can't catch this.

Newtypes make the types distinct. This is a zero-cost abstraction — at runtime
they compile to a single `u64`, but at compile time they are different types:

```rust
fn get_node(id: NodeId) -> Option<Node> { ... }

get_node(EdgeId(5));  // compile error: expected NodeId, found EdgeId
get_node(5_u64);      // compile error: expected NodeId, found u64
get_node(NodeId(5));  // ✓ correct
```

Display format: `NodeId(0)` displays as `"N0"`, `EdgeId(3)` as `"E3"`.

---

## Node

```rust
pub struct Node {
    pub id:         NodeId,
    pub label:      String,
    pub properties: HashMap<String, Value>,
}
```

**Fields:**
- `id` — globally unique, assigned by the database's `IdGenerator`
- `label` — a type tag (e.g. "City", "Person", "Package"). One label per node.
- `properties` — open-ended key-value bag

**Builder pattern:**
```rust
let node = Node::new(id, "City")
    .with_property("name", "London")
    .with_property("population", 9_000_000_i64)
    .with_property("capital", true);
```

**You typically don't construct nodes directly.** Use the database API:
```rust
let id = db.insert_node("City", props)?;
```

---

## Edge

```rust
pub struct Edge {
    pub id:         EdgeId,
    pub source:     NodeId,
    pub target:     NodeId,
    pub label:      String,
    pub weight:     f64,
    pub properties: HashMap<String, Value>,
}
```

**Fields:**
- `id` — globally unique
- `source` / `target` — directed: the edge goes **from** source **to** target
- `label` — relationship type (e.g. "KNOWS", "RAIL", "DEPENDS_ON")
- `weight` — numeric cost. Use `1.0` for unweighted graphs.
- `properties` — arbitrary metadata

**You typically don't construct edges directly.** Use the database API:
```rust
let eid = db.insert_edge(source_id, target_id, "RAIL", 457.0, props)?;
```

---

## Value — the property value type

Properties can hold any of these five types:

```rust
pub enum Value {
    Null,
    Boolean(bool),
    Integer(i64),
    Float(f64),
    Text(String),
}
```

**Why an enum and not `Box<dyn Any>`?**

1. The binary codec can be hand-written (a 1-byte tag + fixed payload)
2. Comparison operations (`=`, `<`, `>`) are straightforward to implement
3. Serialization to JSON is trivial with `serde`
4. The full set of possible types is known at compile time

**`From` conversions** let you write property values without wrapping:
```rust
Value::from("London")       // → Value::Text("London".into())
Value::from(9_000_000_i64)  // → Value::Integer(9_000_000)
Value::from(3.14_f64)       // → Value::Float(3.14)
Value::from(true)           // → Value::Boolean(true)
```

---

## IdGenerator

```rust
pub struct IdGenerator {
    next_node: u64,
    next_edge: u64,
}
```

A simple monotonic counter. Node IDs and edge IDs have separate counters so
they never collide with each other.

**On database reopen**, the generator is seeded from the highest ID found in
storage:
```rust
for node in storage.load_all_nodes()? {
    id_gen.seed_from_node(node.id);
}
```

This guarantees that new IDs never collide with IDs that were assigned in a
previous session, even after a crash.

---

## Property bag design trade-offs

| Choice | What was chosen | Why |
|--------|----------------|-----|
| Schema | Schemaless (open HashMap) | Flexible for education; real DBs add schemas for validation |
| Key type | String | Simple; real DBs often use interned string IDs for size |
| Value types | 5 fixed variants | Keeps the binary codec readable; real DBs add Date, List, Map, etc. |
| Null handling | Explicit `Value::Null` | Consistent with SQL NULL semantics |
| Indexing | No property indexes | Full scan on filter; see [10_scale_and_production.md](10_scale_and_production.md) |

---

## Display output

Each type implements `std::fmt::Display` for human-readable output:

```
Node:   (N0 :City) {name: "London", population: 9000000}
Edge:   (N0) -[E0 :RAIL]-> (N1)
Value:  "London" / 9000000 / 3.14 / true / null
NodeId: N0
EdgeId: E3
```

This format is used by `QueryResult::Display` and in demo output.
