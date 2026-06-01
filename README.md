# AdGraphDb

An educational graph database built from scratch in Rust.

Demonstrates **Ports & Adapters** (hexagonal) architecture, WAL-based
persistence, LRU caching, pluggable graph algorithms, and multiple
interchangeable query DSLs — all implemented without hiding the details.

---

## Quick start

### Prerequisites

- [Rust toolchain](https://rustup.rs/) (stable, 1.70+)

```bash
# Clone
git clone https://github.com/aduwillie/AdGraphDb.git
cd AdGraphDb

# Build
cargo build

# Run the full demo
cargo run

# Run all tests
cargo test

# Run tests with output
cargo test -- --nocapture

# Run a specific test
cargo test test_name
```

---

## What the demo does

`cargo run` executes four demonstrations:

| Demo | What it shows |
|------|--------------|
| 1 — JSON storage | Build a city graph, BFS/DFS/Dijkstra, WAL compaction |
| 2 — Binary storage | Same graph, same operations, swapped storage adapter |
| 3 — Persistence | Write nodes, drop the DB handle, reopen, read them back |
| 4 — Query languages | Same queries in SimpleQuery and CypherLite DSL |

---

## Using the database in your own code

Add to `Cargo.toml`:
```toml
[dependencies]
ad_graph_db = { path = "." }
```

### Minimal example

```rust
use std::collections::HashMap;
use ad_graph_db::{
    adapters::{
        cache::lru::LruCache,
        engine::adjacency_list::AdjacencyListEngine,
        storage::json_file::JsonFileStorage,
    },
    algorithms::dijkstra::Dijkstra,
    core::value::Value,
    database::layered::LayeredGraphDatabase,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Open (or create) the database
    let mut db = LayeredGraphDatabase::open(
        Box::new(JsonFileStorage::open("my_graph.json")?),
        Box::new(LruCache::new(512, 512)),
        Box::new(AdjacencyListEngine::new()),
    )?;

    // Insert nodes
    let london = db.insert_node("City", {
        let mut p = HashMap::new();
        p.insert("name".into(), Value::from("London"));
        p
    })?;
    let paris = db.insert_node("City", {
        let mut p = HashMap::new();
        p.insert("name".into(), Value::from("Paris"));
        p
    })?;

    // Insert an edge
    db.insert_edge(london, paris, "RAIL", 457.0, HashMap::new())?;

    // Read a node
    let node = db.get_node(london)?.unwrap();
    println!("{node}");

    // Find shortest path
    if let Some((path, km)) = db.find_shortest_path(&Dijkstra, london, paris) {
        println!("Path: {path:?}  ({km} km)");
    }

    Ok(())
}
```

### Using query languages

```rust
use ad_graph_db::query::languages::{
    simple::SimpleQueryLanguage,
    cypher_lite::CypherLiteLanguage,
};

// Both produce identical results:
let r1 = db.execute_query(&SimpleQueryLanguage, "MATCH NODE WHERE label = \"City\"")?;
let r2 = db.execute_query(&CypherLiteLanguage,  "MATCH (n:City) RETURN n")?;

println!("{r1}");

// Traversal
let r3 = db.execute_query(&SimpleQueryLanguage, &format!("TRAVERSE BFS FROM {london}"))?;

// Shortest path
let r4 = db.execute_query(&SimpleQueryLanguage, &format!("PATH FROM {london} TO {paris}"))?;
```

### Swapping the storage adapter

```rust
// JSON (human-readable):
Box::new(JsonFileStorage::open("graph.json")?)

// Binary (compact):
Box::new(BinaryFileStorage::open("graph.bin")?)
```

Nothing else changes.

### Swapping the cache

```rust
// LRU with capacity:
Box::new(LruCache::new(node_capacity, edge_capacity))

// No cache (always hits storage — good for testing):
Box::new(NoCache)
```

---

## Running the tests

```bash
# All tests (unit + integration)
cargo test

# Just unit tests (in each source file)
cargo test --lib

# Just integration tests
cargo test --test integration_test

# One test by name
cargo test nodes_survive_reopen_json

# Show println! output
cargo test -- --nocapture

# Run tests sequentially (avoids temp-file collisions on some systems)
cargo test -- --test-threads=1
```

### Test coverage by module

| Module | Tests |
|--------|-------|
| `core/value.rs` | Display, From conversions, equality |
| `core/node.rs` | Builder pattern, Display, clone independence |
| `core/edge.rs` | Display, weight storage, field access |
| `core/id_generator.rs` | Sequential generation, seeding, non-regression |
| `adapters/engine/adjacency_list.rs` | Insert/remove, neighbors, cascades |
| `adapters/cache/lru.rs` | Hit/miss, LRU eviction order, overwrite, clear |
| `adapters/cache/no_cache.rs` | Always-miss behaviour |
| `adapters/storage/json_file.rs` | CRUD, compaction, persistence, reopen |
| `adapters/storage/binary_file.rs` | CRUD, all value types, compaction, reopen |
| `algorithms/bfs.rs` | Level order, cycle safety, unreachable nodes |
| `algorithms/dfs.rs` | Branch order, cycle safety, isolated nodes |
| `algorithms/dijkstra.rs` | Minimum path, no-path, self-path, stale-skip |
| `query/ast.rs` | Filter matching, ComparisonOp, NodeFilter |
| `query/languages/simple.rs` | All commands, case-insensitivity, quoted strings |
| `query/languages/cypher_lite.rs` | All patterns, lexer, operators |
| `tests/integration_test.rs` | Full-stack: CRUD, persistence, algorithms, queries |

---

## Project structure

```
AdGraphDb/
├── Cargo.toml
├── README.md                    ← you are here
├── docs/
│   ├── 01_graph_concepts.md     ← start here if new to graphs
│   ├── 02_architecture.md       ← ports & adapters pattern
│   ├── 03_data_model.md         ← Node, Edge, Value, NodeId
│   ├── 04_persistence_and_wal.md← WAL, write path, compaction
│   ├── 05_storage_formats.md    ← JSON and binary on-disk specs
│   ├── 06_cache_layer.md        ← LRU, eviction policies
│   ├── 07_graph_engine.md       ← adjacency list vs matrix
│   ├── 08_algorithms.md         ← BFS, DFS, Dijkstra in depth
│   ├── 09_query_language.md     ← SimpleQuery & CypherLite DSLs
│   ├── 10_scale_and_production.md ← what it would take to scale
│   ├── 11_adding_adapters.md    ← code templates for extension points
│   ├── 12_query_execution_deep_dive.md ← how queries work end-to-end
│   └── 13_big_o_scale_and_startup.md  ← V/E/degree, billion nodes, structure persistence
├── src/
│   ├── lib.rs
│   ├── main.rs                  ← demo entry point
│   ├── core/                    ← pure domain types (no I/O)
│   │   ├── value.rs
│   │   ├── node.rs
│   │   ├── edge.rs
│   │   ├── error.rs
│   │   └── id_generator.rs
│   ├── ports/                   ← Rust trait interfaces
│   │   ├── storage.rs
│   │   ├── cache.rs
│   │   ├── engine.rs
│   │   ├── algorithm.rs
│   │   └── query_context.rs
│   ├── adapters/                ← concrete implementations
│   │   ├── storage/
│   │   │   ├── json_file.rs     ← NDJSON WAL
│   │   │   └── binary_file.rs   ← hand-written binary WAL
│   │   ├── cache/
│   │   │   ├── lru.rs           ← generation-counter LRU
│   │   │   └── no_cache.rs      ← pass-through
│   │   └── engine/
│   │       └── adjacency_list.rs← HashMap-based adjacency index
│   ├── algorithms/
│   │   ├── bfs.rs
│   │   ├── dfs.rs
│   │   └── dijkstra.rs
│   ├── query/
│   │   ├── ast.rs               ← QueryCommand IR
│   │   ├── executor.rs          ← shared execution engine
│   │   ├── port.rs              ← QueryLanguagePort trait
│   │   ├── result.rs            ← QueryResult type
│   │   └── languages/
│   │       ├── simple.rs        ← SimpleQuery DSL
│   │       └── cypher_lite.rs   ← CypherLite DSL
│   └── database/
│       └── layered.rs           ← LayeredGraphDatabase
└── tests/
    └── integration_test.rs      ← full-stack tests
```

---

## Documentation

Read the docs in order for the best learning experience:

1. **[01_graph_concepts.md](docs/01_graph_concepts.md)** — What is a graph? Key vocabulary.
2. **[02_architecture.md](docs/02_architecture.md)** — Ports & Adapters pattern explained.
3. **[03_data_model.md](docs/03_data_model.md)** — Node, Edge, Value, NodeId types.
4. **[04_persistence_and_wal.md](docs/04_persistence_and_wal.md)** — How data survives restarts.
5. **[05_storage_formats.md](docs/05_storage_formats.md)** — On-disk byte formats.
6. **[06_cache_layer.md](docs/06_cache_layer.md)** — LRU eviction and cache coherence.
7. **[07_graph_engine.md](docs/07_graph_engine.md)** — In-memory adjacency structure.
8. **[08_algorithms.md](docs/08_algorithms.md)** — BFS, DFS, Dijkstra explained.
9. **[09_query_language.md](docs/09_query_language.md)** — SimpleQuery and CypherLite DSLs.
10. **[10_scale_and_production.md](docs/10_scale_and_production.md)** — What changes at scale.
11. **[11_adding_adapters.md](docs/11_adding_adapters.md)** — Code templates for new adapters.
12. **[12_query_execution_deep_dive.md](docs/12_query_execution_deep_dive.md)** — How queries execute end-to-end with traced examples.
13. **[13_big_o_scale_and_startup.md](docs/13_big_o_scale_and_startup.md)** — V/E/degree defined; billion-node impact; structure persistence.
14. *(reserved)*
15. **[15_rust_concepts.md](docs/15_rust_concepts.md)** — Every Rust concept used in this repo, with code examples and rationale.

---

## Design decisions

| Decision | Choice | Why |
|----------|--------|-----|
| Architecture | Ports & Adapters | Swap any layer without touching others |
| Storage | Append-only WAL | Simple crash safety, easy to inspect |
| Cache eviction | Generation-counter LRU | Readable; no unsafe Rust needed |
| Graph structure | Adjacency list (both directions) | O(degree) neighbor queries |
| Algorithms | Zero-sized structs, trait objects | Pluggable at call time |
| Query IR | Shared QueryCommand enum | Multiple DSLs, one executor |
| IDs | Newtype `NodeId(u64)` | Compile-time type safety |
| Serialization | serde\_json (JSON) / hand-written (binary) | Show both approaches |

---

## Dependencies

```toml
serde      = { version = "1", features = ["derive"] }  # JSON serialization
serde_json = "1"                                        # JSON parsing
```

The binary storage adapter, all algorithms, the graph engine, and the cache
are implemented with the standard library only.

---

## License

MIT
