# 02 — Architecture: Ports & Adapters

---

## The core problem

Database code tends to tangle three concerns together:
1. **What** the database does (graph logic, algorithms)
2. **How** it stores data (files, memory, network)
3. **How** it is queried (SQL, DSL, API)

When these concerns are mixed, changing one requires touching all three.
Want to swap from JSON files to a binary format? You're editing graph logic.
Want to add a new query language? You're editing storage code.

**Hexagonal Architecture** (Ports & Adapters), invented by Alistair Cockburn,
solves this by drawing hard boundaries between the concerns.

---

## The hexagon analogy

Imagine the application as a hexagon. Each "face" of the hexagon is a port —
a well-defined boundary where something plugs in. The inside of the hexagon
is pure domain logic that knows nothing about the outside world.

```
                    ┌──────────────────────────────────────────────┐
                    │                                              │
     Query DSL ────►│  StoragePort  CachePort  GraphEnginePort    │
                    │                                              │
     CLI / API ────►│         LayeredGraphDatabase                 │◄──── Tests
                    │                                              │
     File storage ─►│      AlgorithmPort  QueryLanguagePort        │
                    │                                              │
                    └──────────────────────────────────────────────┘
```

The arrows cross the boundary at **ports** (Rust traits).
The implementations that plug in are **adapters**.

---

## Ports in AdGraphDb

A port is a Rust **trait** — a named interface with no implementation.

| Port (trait) | File | Responsibility |
|---|---|---|
| `StoragePort` | `ports/storage.rs` | Persist nodes and edges to durable media |
| `CachePort` | `ports/cache.rs` | Cache hot data in RAM |
| `GraphEnginePort` | `ports/engine.rs` | Maintain in-memory adjacency structure |
| `TraversalAlgorithm` | `ports/algorithm.rs` | Visit all reachable nodes |
| `ShortestPathAlgorithm` | `ports/algorithm.rs` | Find minimum-weight paths |
| `QueryLanguagePort` | `query/port.rs` | Parse a DSL string and execute it |
| `DatabaseContext` | `ports/query_context.rs` | What parsers see of the database |

---

## Adapters in AdGraphDb

An adapter is a concrete struct that **implements** one of the port traits.

| Adapter | Implements | File |
|---------|-----------|------|
| `JsonFileStorage` | `StoragePort` | `adapters/storage/json_file.rs` |
| `BinaryFileStorage` | `StoragePort` | `adapters/storage/binary_file.rs` |
| `LruCache` | `CachePort` | `adapters/cache/lru.rs` |
| `NoCache` | `CachePort` | `adapters/cache/no_cache.rs` |
| `AdjacencyListEngine` | `GraphEnginePort` | `adapters/engine/adjacency_list.rs` |
| `BreadthFirstSearch` | `TraversalAlgorithm` | `algorithms/bfs.rs` |
| `DepthFirstSearch` | `TraversalAlgorithm` | `algorithms/dfs.rs` |
| `Dijkstra` | `ShortestPathAlgorithm` | `algorithms/dijkstra.rs` |
| `SimpleQueryLanguage` | `QueryLanguagePort` | `query/languages/simple.rs` |
| `CypherLiteLanguage` | `QueryLanguagePort` | `query/languages/cypher_lite.rs` |

---

## The assembly point: LayeredGraphDatabase

`LayeredGraphDatabase` (in `database/layered.rs`) is the only place that
holds concrete types. It wires the three primary adapters together:

```rust
pub struct LayeredGraphDatabase {
    engine:  Box<dyn GraphEnginePort>,   // structure
    cache:   Box<dyn CachePort>,         // RAM read layer
    storage: Box<dyn StoragePort>,       // disk durability
    id_generator: IdGenerator,           // monotonic IDs
}
```

All three are held as **trait objects** (`Box<dyn Trait>`), so the struct
has no knowledge of which concrete adapter is inside.

---

## Swapping an adapter: zero changes to anything else

### Swap storage from JSON to binary

```rust
// Before:
let storage = JsonFileStorage::open("graph.json")?;

// After:
let storage = BinaryFileStorage::open("graph.bin")?;

// Everything else is identical:
let db = LayeredGraphDatabase::open(
    Box::new(storage),
    Box::new(LruCache::new(512, 512)),
    Box::new(AdjacencyListEngine::new()),
)?;
```

### Swap the cache from LRU to no-cache

```rust
// Change one line:
Box::new(NoCache)  // instead of Box::new(LruCache::new(512, 512))
```

The graph logic, algorithms, and query languages are completely unchanged.

---

## Dependency rules (enforced by Rust's module system)

```
core  ←  ports  ←  adapters
                ←  algorithms
                ←  query
                       ←  database   (wires everything)
```

- `core` imports nothing from this crate
- `ports` imports only `core`
- `adapters`, `algorithms`, and `query` import `ports` and `core`
- `database` imports everything and is the final assembly point

If you try to import `adapters` from `core`, the compiler stops you.
This is enforced, not just a convention.

---

## Why this matters for education

The pattern separates **three independent learning axes**:

1. **Storage** — how databases persist data (WAL, B-trees, LSM trees)
2. **Graph theory** — algorithms, representations, complexity
3. **Query languages** — parsing, compiling, optimising

You can study and experiment with any axis without touching the others.
Want to learn about LRU eviction? Change only `adapters/cache/lru.rs`.
Want to implement A*? Add one file in `algorithms/`.
Want to add a SQL-like query language? Add one file in `query/languages/`.

---

## Full component diagram

```
┌──────────────────────────────────────────────────────────────────┐
│                     src/database/layered.rs                      │
│                   LayeredGraphDatabase                           │
│                                                                  │
│  ┌───────────────┐  ┌──────────────┐  ┌────────────────────┐   │
│  │  GraphEngine  │  │  CachePort   │  │   StoragePort      │   │
│  │     Port      │  │              │  │                    │   │
│  └──────┬────────┘  └──────┬───────┘  └─────────┬──────────┘   │
└─────────┼──────────────────┼────────────────────┼──────────────┘
          │                  │                    │
  ┌───────▼──────┐  ┌────────▼────────┐  ┌───────▼──────────────┐
  │  Adjacency   │  │ LruCache /      │  │ JsonFileStorage /    │
  │  ListEngine  │  │ NoCache         │  │ BinaryFileStorage    │
  └──────────────┘  └─────────────────┘  └──────────────────────┘

Algorithms (pluggable at call time, not at construction):
  BreadthFirstSearch  implements TraversalAlgorithm
  DepthFirstSearch    implements TraversalAlgorithm
  Dijkstra            implements ShortestPathAlgorithm

Query (pluggable at call time):
  SimpleQueryLanguage   implements QueryLanguagePort
  CypherLiteLanguage    implements QueryLanguagePort
  └── both share: QueryCommand IR + executor.rs
```
