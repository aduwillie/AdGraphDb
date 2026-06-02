# 12 — Query Execution Deep Dive

> **The central question this document answers:**
> *When you search a graph database, where do you start — and how does the
> system find what you asked for?*

This is the hardest question to answer about any database.  In a relational
database the answer is "start from a table."  In a graph database the answer
is more subtle and more interesting.

> **Big-O notation used in this document** — quick reference:
>
> | Symbol | Meaning |
> |--------|---------|
> | **V** | Total number of **vertices** (nodes) in the graph |
> | **E** | Total number of **edges** |
> | **degree** | Number of edges on one *specific* node (not the whole graph) |
> | **O(1)** | Constant time — does not grow with V or E |
> | **O(V)** | Grows linearly with total nodes — doubles when V doubles |
> | **O(V + E)** | Visits every node once and every edge once |
> | **O(degree)** | Proportional only to *one node's* connections — independent of V |
>
> See [13_big_o_scale_and_startup.md](13_big_o_scale_and_startup.md) for a full
> explanation of these terms, what they mean at a billion nodes, and how the graph
> structure is persisted and rebuilt at startup.

---

## Part 1 — The fundamental problem

### Relational databases have a clear starting point

In SQL, every query starts with a table.  A table is a flat list of rows.
"Find all users named Alice" means: scan the `users` table, keep rows where
`name = 'Alice'`.  There is always one entry point: the table.

```sql
SELECT * FROM users WHERE name = 'Alice';
-- Entry point: the "users" table (all rows)
```

### Graph databases do not have tables

A graph database has nodes and edges — but no table to scan from.
Nodes can have any label.  Edges connect any two nodes.
The data is a *web*, not a grid.

This creates a problem:

**To do anything useful, you need a starting node.**
Every graph algorithm (BFS, DFS, Dijkstra) takes a `start: NodeId` as its
first argument.  But how do you know which node to start from?

There are exactly two answers:

| Strategy | How | When to use |
|----------|-----|-------------|
| **Global scan** | Load all nodes, filter by label/property | You don't have a specific node in mind |
| **Direct anchor** | You already know the node's ID | You're following up from a previous query |

These are the two fundamental query modes in every graph database.

---

## Part 2 — The two query modes in AdGraphDb

### Mode A: Global scan (find the starting set)

A global scan loads all nodes or edges, then filters them in memory.

```
MATCH NODE WHERE label = "City"
MATCH NODE WHERE label = "City" AND props.population > 1000000
MATCH EDGE WHERE label = "RAIL" AND weight < 500
```

This is the "table scan" of graph databases.  It is how you turn an open
question ("which nodes are cities?") into a concrete set of node IDs that you
can then use as starting points for traversal.

**What happens internally — with label filter (fast path):**
```
executor::execute(MatchNodes(filter { label: Some("City"), ... }), ctx)
  ctx.get_nodes_by_label("City")   ← O(1) LabelIndex lookup → [N0, N1, N3]
  for each candidate (N0, N1, N3 only):
    filter.matches(node)           ← check remaining property conditions
  return matched nodes
```

**What happens internally — without label filter (full scan):**
```
executor::execute(MatchNodes(filter { label: None, ... }), ctx)
  ctx.get_all_nodes()              ← load every node: O(N)
  for each node:
    filter.matches(node)           ← check all conditions
  return matched nodes
```

The cost is **O(label_count)** when a label filter is present (using the
in-memory `LabelIndex`), or **O(N)** when no label filter is given.
Property-only filters still require a full scan — a BTree property index
would reduce that to O(log N + results).  See Part 6 and
[10_scale_and_production.md](10_scale_and_production.md).

### Mode B: Direct anchor (you know the ID)

If you already know a node's ID, you can jump directly to it:

```
GET NODE N5               ← O(1): direct hash lookup
TRAVERSE BFS FROM N5      ← starts at N5, follows edges outward
PATH FROM N5 TO N12       ← finds cheapest path between two specific nodes
```

This is the native strength of graph databases.  Once you have your anchor
node, following its edges is **O(degree)** — proportional to how many
connections that specific node has, completely independent of how large the
overall graph is.

The typical pattern is: **global scan to find the anchor, then local
traversal to explore.**

```
Step 1 (global scan):  MATCH NODE WHERE props.name = "London"  → N0
Step 2 (local anchor): TRAVERSE BFS FROM N0
```

---

## Part 3 — The complete query pipeline

Every query goes through the same five-stage pipeline regardless of which
DSL you use.

```
                    ┌──────────────────────────────────────────────┐
  "MATCH (n:City)   │                                              │
   WHERE            │  Stage 1: Tokenise                           │
   n.population     │  Split the string into meaningful pieces     │
   > 1000000        │  (keywords, identifiers, operators, values)  │
   RETURN n"        │                                              │
         │          └──────────────────────┬───────────────────────┘
         │                                 │ tokens
         │          ┌──────────────────────▼───────────────────────┐
         │          │  Stage 2: Parse                              │
         │          │  Apply grammar rules to produce a structured │
         │          │  representation of the query's intent        │
         │          └──────────────────────┬───────────────────────┘
         │                                 │ QueryCommand (IR)
         │          ┌──────────────────────▼───────────────────────┐
         │          │  Stage 3: Execute                            │
         │          │  The executor maps the IR variant to the     │
         │          │  appropriate DatabaseContext calls           │
         │          └──────────────────────┬───────────────────────┘
         │                                 │ calls to DatabaseContext
         │          ┌──────────────────────▼───────────────────────┐
         │          │  Stage 4: Data retrieval                     │
         │          │  LayeredGraphDatabase reads from:            │
         │          │    Cache (RAM)  →  Storage (disk)            │
         │          │    Engine (adjacency index)                  │
         │          └──────────────────────┬───────────────────────┘
         │                                 │ raw Node/Edge data
         │          ┌──────────────────────▼───────────────────────┐
         │          │  Stage 5: Filter & assemble                  │
         │          │  Apply NodeFilter / EdgeFilter               │
         │          │  Return QueryResult                          │
         │          └──────────────────────────────────────────────┘
```

### Stage 1: Tokenise

The raw query string is broken into tokens.  The two parsers do this
differently because they have different grammars:

**SimpleQuery** — whitespace-split, then classify each token:
```
"MATCH NODE WHERE label = \"City\""
   ↓
["MATCH", "NODE", "WHERE", "label", "=", "\"City\""]
```

**CypherLite** — character-by-character lexer, produces typed tokens:
```
"MATCH (n:City) WHERE n.population > 1000000 RETURN n"
   ↓
[Keyword("MATCH"), LParen, Ident("n"), Colon, Ident("City"), RParen,
 Keyword("WHERE"), Ident("n"), Dot, Ident("population"),
 Gt, IntLit(1000000), Keyword("RETURN"), Ident("n")]
```

The lexer's job is purely mechanical: it does not understand grammar,
it just identifies where one token ends and the next begins.

### Stage 2: Parse

The parser applies grammar rules to the token stream and produces a
`QueryCommand` — the language-independent intermediate representation.

Both parsers produce the *same* `QueryCommand` for equivalent queries:

```rust
// "MATCH NODE WHERE label = \"City\" AND props.population > 1000000"
// "MATCH (n:City) WHERE n.population > 1000000 RETURN n"
//   ↓ both produce:

QueryCommand::MatchNodes(NodeFilter {
    label: Some("City".into()),
    property_conditions: vec![
        PropertyCondition {
            key:   "population".into(),
            op:    ComparisonOp::Gt,
            value: Value::Integer(1_000_000),
        }
    ],
})
```

Once in `QueryCommand` form, neither the executor nor the database knows
or cares which DSL was used.

**How the SimpleQuery parser works (recursive descent):**

```rust
fn parse(&mut self) -> Result<QueryCommand, GraphError> {
    let kw = self.keyword()?;   // read the first token
    match kw.as_str() {
        "MATCH"    => self.parse_match(),
        "GET"      => self.parse_get(),
        "TRAVERSE" => self.parse_traverse(),
        "PATH"     => self.parse_path(),
        "COUNT"    => self.parse_count(),
        other => Err(parse_error(...))
    }
}

fn parse_match(&mut self) -> Result<QueryCommand, GraphError> {
    let kind = self.keyword()?;  // "NODE" or "EDGE"
    match kind.as_str() {
        "NODE" => {
            let filter = self.parse_optional_node_filter()?;
            Ok(QueryCommand::MatchNodes(filter))
        }
        "EDGE" => { ... }
    }
}
```

Each method handles one grammar rule.  The parser is a cascade of small,
readable functions — no regex, no parser combinator library.

### Stage 3: Execute

The executor (`query/executor.rs`) is a single `match` statement over
`QueryCommand` variants.  It dispatches to `DatabaseContext` methods:

```rust
pub fn execute(command: QueryCommand, ctx: &mut dyn DatabaseContext)
    -> Result<QueryResult, GraphError>
{
    match command {
        QueryCommand::MatchNodes(filter) => {
            let all = ctx.get_all_nodes()?;          // Stage 4
            let matched = all.into_iter()
                .filter(|n| filter.matches(n))       // Stage 5
                .collect();
            Ok(QueryResult::Nodes(matched))
        }

        QueryCommand::Traverse { kind, start } => {
            let ids = match kind {
                TraversalKind::Bfs => ctx.traverse_bfs(start),
                TraversalKind::Dfs => ctx.traverse_dfs(start),
            };
            Ok(QueryResult::Traversal(ids))
        }

        QueryCommand::ShortestPath { start, goal } => {
            match ctx.shortest_path_dijkstra(start, goal) {
                Some((nodes, weight)) => Ok(QueryResult::Path { nodes, total_weight: weight }),
                None                  => Ok(QueryResult::Empty),
            }
        }
        // ...
    }
}
```

The executor knows nothing about storage, caching, or the graph engine.
It only speaks `DatabaseContext`.

### Stage 4: Data retrieval

`LayeredGraphDatabase` implements `DatabaseContext`.  When the executor
calls `ctx.get_all_nodes()`, the database:

```rust
// From database/layered.rs
pub fn all_nodes(&mut self) -> Result<Vec<Node>, GraphError> {
    let ids = self.engine.all_node_ids();   // ← from RAM (adjacency index)
    let mut nodes = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(node) = self.get_node(id)? {  // ← cache → storage
            nodes.push(node);
        }
    }
    Ok(nodes)
}
```

The **engine** answers "which node IDs exist?" (RAM, O(V)).
The **cache** answers "what are their properties?" (RAM, O(1) per hit).
The **storage** fills cache misses (disk, O(WAL_size) first time, O(1) after).

### Stage 5: Filter

The filter (`NodeFilter::matches`) is applied after retrieval.
It checks every returned node against the conditions:

```rust
impl NodeFilter {
    pub fn matches(&self, node: &Node) -> bool {
        // Check label first (cheap string equality)
        if let Some(ref expected) = self.label {
            if &node.label != expected { return false; }
        }
        // Then check all property conditions (HashMap lookups)
        self.property_conditions
            .iter()
            .all(|cond| cond.matches_properties(&node.properties))
    }
}

impl PropertyCondition {
    pub fn matches_properties(&self, props: &HashMap<String, Value>) -> bool {
        match props.get(&self.key) {
            None        => false,   // property doesn't exist → no match
            Some(actual) => self.op.compare_values(actual, &self.value),
        }
    }
}
```

**Short-circuit evaluation**: if the label check fails, the property
conditions are never evaluated.  Within property conditions, the iterator
stops at the first failure (Rust's `.all()` short-circuits).

---

## Part 4 — Trace every query type

### 4a. Global node scan

```
Query:   MATCH NODE WHERE label = "City" AND props.population > 1000000

Stage 1  tokenise  →  [MATCH, NODE, WHERE, label, =, "City", AND, props.population, >, 1000000]

Stage 2  parse     →  QueryCommand::MatchNodes(NodeFilter {
                           label: Some("City"),
                           property_conditions: [
                               PropertyCondition { key: "population", op: Gt, value: Integer(1000000) }
                           ]
                       })

Stage 3  execute   →  ctx.get_all_nodes()

Stage 4  retrieve  →  engine.all_node_ids()  →  [N0, N1, N2, N3]
                      for N0: cache miss → storage.load_node(N0) → cache.put_node(N0)
                      for N1: cache miss → storage.load_node(N1) → cache.put_node(N1)
                      ...

Stage 5  filter    →  N0 (London,  pop=9_000_000)  label=City ✓  pop>1M ✓  → keep
                      N1 (Paris,   pop=2_100_000)  label=City ✓  pop>1M ✓  → keep
                      N2 (Lewes,   pop=17_000)     label=City ✓  pop>1M ✗  → discard
                      N3 (Brussels,pop=1_200_000)  label=City ✓  pop>1M ✓  → keep

Result:  QueryResult::Nodes([N0, N1, N3])
```

**Cost breakdown:**
```
engine.all_node_ids()     O(V) — HashMap key collection
storage.load_node × V     O(WAL_size) first call; O(1) per node after cache warm
filter.matches × V        O(V × conditions)
─────────────────────────────────────────
Total first call:         O(V × WAL_size)   ← catastrophic at V = 1 billion
Total after cache warm:   O(V)              ← still linear; needs indexes for scale
```

> ⚠ **Scale note**: this is a full scan — every node is examined regardless
> of how many match.  For V = 1 billion, this query is unusable without a
> label or property index.  See [13_big_o_scale_and_startup.md](13_big_o_scale_and_startup.md).

### 4b. Direct point lookup

```
Query:   GET NODE N2

Stage 1  tokenise  →  [GET, NODE, N2]
Stage 2  parse     →  QueryCommand::GetNode(NodeId(2))
Stage 3  execute   →  ctx.get_node(NodeId(2))

Stage 4  retrieve  →  cache.get_node(N2)     ← HIT (if previously loaded)
                      → return immediately

                   OR

                      cache.get_node(N2)     ← MISS
                      storage.load_node(N2)  ← WAL replay for this ID
                      cache.put_node(N2)
                      → return

Stage 5  (no filter needed)

Result:  QueryResult::SingleNode(Some(Node { id: N2, label: "City", ... }))
```

**Cost breakdown:**
```
cache hit:   O(1)
cache miss:  O(WAL_size) first time; O(1) after
```

This is the fastest possible query.  Use it when you already know the ID.

### 4c. BFS traversal

```
Query:   TRAVERSE BFS FROM N0

Stage 1  tokenise  →  [TRAVERSE, BFS, FROM, N0]
Stage 2  parse     →  QueryCommand::Traverse { kind: Bfs, start: NodeId(0) }
Stage 3  execute   →  ctx.traverse_bfs(NodeId(0))
                      → BreadthFirstSearch.traverse(engine, NodeId(0))

Stage 4  (engine only — no property data loaded)

  queue = [N0],  visited = {N0}

  Iteration 1:  pop N0
                engine.neighbors_outgoing(N0)  →  [N1(E0,457km), N2(E1,370km)]
                N1 not visited → push; N2 not visited → push
                queue = [N1, N2],  visited = {N0, N1, N2}

  Iteration 2:  pop N1
                engine.neighbors_outgoing(N1)  →  [N2(E2,265km), N3(E3,1054km)]
                N2 already visited → skip; N3 not visited → push
                queue = [N2, N3],  visited = {N0, N1, N2, N3}

  Iteration 3:  pop N2
                engine.neighbors_outgoing(N2)  →  [N3(E4,174km)]
                N3 already visited → skip
                queue = [N3]

  Iteration 4:  pop N3
                engine.neighbors_outgoing(N3)  →  []
                queue = []

  Result: [N0, N1, N2, N3]

Stage 5  (no filter)

Result:  QueryResult::Traversal([N0, N1, N2, N3])
```

**Key insight**: BFS touches the engine only — **zero disk reads**.
All structural data (who connects to whom) is in the adjacency list in RAM.
Property data (name, population) is not loaded unless you explicitly ask.

**Cost breakdown:**
```
engine.neighbors_outgoing × (V + E)   O(V + E)  — all in RAM
No storage or cache calls during traversal
─────────────────────────────────────────
Total:  O(V + E)  in RAM
```

### 4d. Shortest path (Dijkstra)

```
Graph for this example:
  N0 ──1.0──> N1 ──2.0──> N3
  N0 ──10.0─> N2 ──1.0──> N3

Query:   PATH FROM N0 TO N3

Stage 1  tokenise  →  [PATH, FROM, N0, TO, N3]
Stage 2  parse     →  QueryCommand::ShortestPath { start: NodeId(0), goal: NodeId(3) }
Stage 3  execute   →  ctx.shortest_path_dijkstra(N0, N3)
                      → Dijkstra.find_shortest_path(engine, N0, N3)

Stage 4  (engine only — no property data)

  distance = { N0: 0.0, N1: ∞, N2: ∞, N3: ∞ }
  heap     = [(0.0, N0)]

  Pop (0.0, N0):
    neighbors_outgoing(N0) = [N1(1.0), N2(10.0)]
    N1: 0.0+1.0=1.0  < ∞  → distance[N1]=1.0, predecessor[N1]=N0, push (1.0, N1)
    N2: 0.0+10.0=10.0 < ∞ → distance[N2]=10.0, predecessor[N2]=N0, push (10.0, N2)
    heap = [(1.0, N1), (10.0, N2)]

  Pop (1.0, N1):
    neighbors_outgoing(N1) = [N3(2.0)]
    N3: 1.0+2.0=3.0  < ∞  → distance[N3]=3.0, predecessor[N3]=N1, push (3.0, N3)
    heap = [(3.0, N3), (10.0, N2)]

  Pop (3.0, N3):
    N3 == goal!  Stop.

  reconstruct_path(predecessor, N0, N3):
    N3 → predecessor[N3]=N1 → predecessor[N1]=N0 (=start, stop)
    reversed: [N0, N1, N3]

Stage 5  (no filter)

Result:  QueryResult::Path { nodes: [N0, N1, N3], total_weight: 3.0 }

Note: the path N0→N2→N3 (cost 11.0) was never fully explored —
Dijkstra found the cheaper path first.
```

**Cost breakdown:**
```
BinaryHeap operations:  O((V + E) log V)  — all in RAM
No storage or cache calls during path finding
─────────────────────────────────────────
Total:  O((V + E) log V)  in RAM
```

---

## Part 5 — Why the engine and storage are separate layers

A critical design decision is that **traversal algorithms only see the engine**,
not the cache or storage.  This is intentional:

```rust
// TraversalAlgorithm receives the engine, not the database
fn traverse(&self, engine: &dyn GraphEnginePort, start: NodeId) -> Vec<NodeId>
//                         ^^^^^^^^^^^^^^^^^^^^
//                         NOT &mut LayeredGraphDatabase
```

**Why?**

1. **Speed**: The engine is entirely in RAM.  A BFS over 1 million nodes
   makes 1 million `neighbors_outgoing` calls — all HashMap lookups.
   If each call hit the disk, it would be millions of I/O operations.

2. **Separation of concerns**: Algorithms are pure computation.  They should
   not need to know about files, caches, or serialization.

3. **Testability**: You can test BFS by giving it a mock engine —
   no file system, no disk, no setup.

**The trade-off**: algorithms can only see *which* nodes are connected
(IDs and weights), not *what* the nodes contain (name, properties).
If an algorithm needs property data during traversal (e.g. "follow edges
only if the target city has population > 1M"), you would need to modify
the API to pass the full database — or pre-filter the graph before
running the algorithm.

---

## Part 6 — The "where to start" problem in practice

### The two-step pattern

Almost every non-trivial graph query follows this shape:

```
Step 1: Find anchor node(s)     ← global scan (O(N))
Step 2: Traverse from anchor    ← local exploration (O(V+E) from anchor)
```

Example workflow:

```rust
// Step 1: find anchor (returns a set)
let cities = db.execute_query(
    &SimpleQueryLanguage,
    "MATCH NODE WHERE label = \"City\" AND props.name = \"London\"",
)?;

// Step 2: extract the ID
let london_id = match cities {
    QueryResult::Nodes(nodes) => nodes[0].id,
    _ => panic!("expected nodes"),
};

// Step 3: traverse from anchor (local, fast)
let reachable = db.execute_query(
    &SimpleQueryLanguage,
    &format!("TRAVERSE BFS FROM {london_id}"),
)?;

// Step 4: find shortest path (local, fast)
let path = db.execute_query(
    &SimpleQueryLanguage,
    &format!("PATH FROM {london_id} TO {berlin_id}"),
)?;
```

### When you already know the ID

If you store node IDs in your application (e.g. "London is always N0 in
this dataset"), you can skip the global scan entirely:

```rust
let london_id = NodeId(0);  // known from previous session or external mapping
let path = db.execute_query(&SimpleQueryLanguage, "PATH FROM N0 TO N5")?;
```

This is the fastest possible graph query: O((V+E) log V) entirely in RAM.

### The index solution (for large graphs)

For large graphs the O(N) global scan is too slow.  The solution is a
secondary index that maps label/property values directly to node IDs:

```
Label index:
  "City"   →  { N0, N1, N3, N7, N12, ... }
  "Person" →  { N2, N4, N5, N6, N8, ... }

Property index (BTree on "name"):
  "Amsterdam" → { N3 }
  "Berlin"    → { N2 }
  "Brussels"  → { N3 }
  "London"    → { N0 }
```

With a label index, `MATCH NODE WHERE label = "City"` becomes:

```
O(1)  →  index.get("City")  →  set of NodeIds
```

With a property index, `MATCH NODE WHERE props.name = "London"` becomes:

```
O(log N)  →  btree.get("London")  →  { N0 }
```

Instead of O(N), these are O(1) and O(log N) respectively.

**The label index is now implemented** in `src/adapters/index/label_index.rs`.
`MATCH NODE WHERE label = "City"` uses it automatically — the executor calls
`ctx.get_nodes_by_label("City")` which does a single HashMap lookup.

A BTree property index (for range queries like `population > 1_000_000`) is
the next step — see [10_scale_and_production.md](10_scale_and_production.md).

---

## Part 7 — Index-free adjacency: the graph database superpower

The key performance claim of graph databases is called **index-free adjacency**.

In a relational database, finding the friends of a user requires a join:

```sql
-- SQL: "who are Alice's friends?"
SELECT u.name
FROM users u
JOIN friendships f ON f.friend_id = u.id
WHERE f.user_id = alice_id;
-- Cost: O(log N) for the index scan on friendships + O(degree) for the join
-- For "friends of friends": O(degree × log N) — two index scans
-- For "friends of friends of friends": O(degree² × log N) — three index scans
```

In a graph database with index-free adjacency:

```
// "who are Alice's friends?"
engine.neighbors_outgoing(alice_id)   →  [Bob, Carol, Dave]
// Cost: O(degree)  — one HashMap lookup

// "friends of friends"
for each friend:
    engine.neighbors_outgoing(friend)   →  their friends
// Cost: O(degree²)  — still no index, just pointer following

// "friends of friends of friends" (BFS depth 3)
BFS with max_depth = 3
// Cost: O(degree³)
```

The difference is that in the graph database, **following an edge is O(1)**:
it is a pointer to a `Vec<Neighbor>` that already lives in RAM.
No secondary index lookup, no join table scan — just a direct memory access.

This is why graph databases dramatically outperform SQL for **multi-hop
relationship queries**.  The advantage grows with hop depth:

| Hops | SQL cost | Graph DB cost |
|------|----------|---------------|
| 1 | O(log N + degree) | O(degree) |
| 2 | O(degree × log N) | O(degree²) |
| 3 | O(degree² × log N) | O(degree³) |
| k | O(degreeᵏ⁻¹ × log N) | O(degreeᵏ) |

The `log N` factor in SQL represents the repeated index scans across
the friendship table.  Graph databases eliminate this by storing adjacency
directly on the node.

In AdGraphDb the adjacency lives in `AdjacencyListEngine::outgoing`:

```rust
outgoing: HashMap<NodeId, Vec<Neighbor>>
//                          ^^^^^^^^^^^
//                          Direct list of neighbors — no secondary lookup
```

`engine.neighbors_outgoing(N0)` is:

```rust
self.outgoing.get(&id).cloned().unwrap_or_default()
// One HashMap lookup → returns a Vec<Neighbor> already in RAM
// Cost: O(1)
```

---

## Part 8 — What happens when query data crosses layers

### Query: MATCH NODE (involves all three layers)

```
                 ┌──────────────────────┐
                 │  Query executor      │
                 │  MatchNodes(filter)  │
                 └──────────┬───────────┘
                            │ ctx.get_all_nodes()
                 ┌──────────▼───────────┐
                 │  LayeredGraphDatabase│
                 │  all_nodes()         │
                 └──────────┬───────────┘
                            │ engine.all_node_ids()
         ┌──────────────────▼──────────────────────┐
         │  GraphEngine (RAM)                      │
         │  outgoing.keys() → [N0, N1, N2, N3]    │
         └──────────────────┬──────────────────────┘
                            │ for each id: get_node(id)
                 ┌──────────▼───────────┐
                 │  Cache (RAM)         │
                 │  get_node(N0) → HIT  │
                 │  get_node(N1) → MISS │
                 │  get_node(N2) → MISS │
                 └──────────┬───────────┘
                            │ on miss: storage.load_node(id)
                 ┌──────────▼───────────┐
                 │  Storage (disk)      │
                 │  replay WAL          │
                 │  find N1 record      │
                 │  cache.put_node(N1)  │
                 └──────────────────────┘
```

### Query: TRAVERSE BFS (engine only)

```
                 ┌──────────────────────┐
                 │  Query executor      │
                 │  Traverse{Bfs, N0}   │
                 └──────────┬───────────┘
                            │ ctx.traverse_bfs(N0)
                 ┌──────────▼───────────┐
                 │  LayeredGraphDatabase│
                 │  self.traverse(      │
                 │    &BFS, N0)         │
                 └──────────┬───────────┘
                            │ BFS.traverse(engine, N0)
         ┌──────────────────▼──────────────────────┐
         │  GraphEngine (RAM) ← ONLY THIS LAYER    │
         │  contains_node(N0) ✓                    │
         │  neighbors_outgoing(N0) → [N1, N2]      │
         │  neighbors_outgoing(N1) → [N2, N3]      │
         │  neighbors_outgoing(N2) → [N3]          │
         │  neighbors_outgoing(N3) → []            │
         └──────────────────────────────────────────┘
         Cache: NOT CALLED
         Storage: NOT CALLED
```

Traversal is **entirely in RAM**, touching only the engine.
This is the key performance property of the layered design.

---

## Part 9 — Common query patterns and their costs

### "Find a specific thing" (point lookup)

```
GET NODE N5                          O(1) cache / O(WAL) miss
GET EDGE E3                          O(1) cache / O(WAL) miss
```

Use when you know the ID.  Fastest possible query.

### "Find all things of a type" (label lookup — now O(1))

```
MATCH NODE WHERE label = "City"      O(label_count) — LabelIndex HashMap lookup ✓
MATCH EDGE WHERE label = "RAIL"      O(E) — edge label index not yet implemented
```

Node label queries use the in-memory `LabelIndex` automatically.
Only nodes with that label are loaded; all others are skipped entirely.

### "Find things with a property value" (property filter — still O(N))

```
MATCH NODE WHERE props.name = "London"      O(N) — full scan; no property index yet
MATCH EDGE WHERE weight < 500               O(E) — full scan
```

Property filters still require a full scan.  A BTree property index would
reduce this to O(log N + results).  See [10_scale_and_production.md](10_scale_and_production.md).

### "Explore from a starting node" (traversal)

```
TRAVERSE BFS FROM N0        O(V + E) in RAM — visits all reachable nodes
TRAVERSE DFS FROM N0        O(V + E) in RAM — same, different order
```

Use to answer "what can I reach from here?" or "what nodes are nearby?"

### "Find the cheapest route" (shortest path)

```
PATH FROM N0 TO N5          O((V + E) log V) in RAM — Dijkstra
```

Use for routing, planning, dependency resolution.

### "Find things, then explore" (the typical pattern)

```
Step 1: MATCH NODE WHERE label = "City"    O(N) — find candidates
Step 2: TRAVERSE BFS FROM N_london         O(V + E) in RAM — explore
```

The global scan in step 1 is the bottleneck.  Reduce it with indexes.

---

## Part 10 — Mental model: two separate questions

When you query a graph database, you are always answering one or both of:

**Question 1: What nodes exist that match these criteria?**
(Global scan, set-based, like SQL SELECT)

**Question 2: Starting from these nodes, what can I reach?**
(Local traversal, pointer-following, unique to graph databases)

The second question is what graph databases are uniquely good at.
The first question is where they still need indexes, just like relational databases.

A well-designed graph query combines both:

```
1. Use indexes (or ID lookups) to find a small starting set
2. Use traversal to explore the graph from that set
```

When the starting set is one node and the traversal explores many hops,
a graph database dramatically outperforms SQL — because graph databases
eliminate the repeated index lookups that SQL needs at each hop.

---

## Summary

| Concept | What it means |
|---------|--------------|
| Label lookup | O(1) LabelIndex lookup when label filter present — O(label_count) |
| Global scan | Load all nodes/edges, filter in memory — O(N); used when no label filter |
| Point lookup | Load one node by ID — O(1) with cache |
| Anchor node | The starting node for a traversal |
| Traversal | Follow edges from an anchor — O(V+E) in RAM |
| Shortest path | Find cheapest route between two anchors — O((V+E) log V) in RAM |
| Index-free adjacency | Neighbors stored directly on the node — O(1) edge follow |
| Two-step pattern | Global scan to find anchor → local traversal to explore |
| Layer separation | Engine (structure) never touches storage (properties) |
| Filter in executor | Applied after retrieval, before returning results |
| Short-circuit | Label checked before properties; conditions stop at first failure |

The query execution pipeline in AdGraphDb follows this path for every query:

```
string → tokenise → parse → QueryCommand IR → execute → retrieve (engine/cache/storage) → filter → QueryResult
```

Understanding which layer answers which question is the key to understanding
why the database performs the way it does — and where to look when you need
to make it faster.
