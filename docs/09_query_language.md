# 09 — Query Language

AdGraphDb supports multiple pluggable query DSLs that share a common
intermediate representation (IR) and executor.

---

## Design philosophy

A query language has three separable concerns:

```
1. Surface syntax  —  what the user types
2. Intermediate representation (IR)  —  what the system executes
3. Execution  —  how the IR is run against the database
```

AdGraphDb separates these explicitly:

```
User string
    │
    ▼ parser (language-specific)
QueryCommand (IR)         ← shared across all languages
    │
    ▼ executor (shared)
QueryResult
```

Adding a new language = writing a new parser. The IR and executor are unchanged.

---

## QueryLanguagePort

```rust
pub trait QueryLanguagePort {
    fn language_name(&self) -> &str;
    fn execute(
        &self,
        query: &str,
        context: &mut dyn DatabaseContext,
    ) -> Result<QueryResult, GraphError>;
}
```

The `DatabaseContext` trait exposes what queries need:

```rust
pub trait DatabaseContext {
    fn get_node(&mut self, id: NodeId) -> Result<Option<Node>, GraphError>;
    fn get_all_nodes(&mut self) -> Result<Vec<Node>, GraphError>;

    /// Fast-path: returns only nodes whose label matches, using the in-memory
    /// LabelIndex (O(label_count)) instead of a full scan (O(N)).
    fn get_nodes_by_label(&mut self, label: &str) -> Result<Vec<Node>, GraphError>;

    fn get_edge(&mut self, id: EdgeId) -> Result<Option<Edge>, GraphError>;
    fn get_all_edges(&mut self) -> Result<Vec<Edge>, GraphError>;
    fn node_count(&self) -> usize;
    fn edge_count(&self) -> usize;
    fn traverse_bfs(&self, start: NodeId) -> Vec<NodeId>;
    fn traverse_dfs(&self, start: NodeId) -> Vec<NodeId>;
    fn shortest_path_dijkstra(&self, start: NodeId, goal: NodeId) -> Option<(Vec<NodeId>, f64)>;
}
```

---

## QueryCommand — the intermediate representation

```rust
pub enum QueryCommand {
    MatchNodes(NodeFilter),
    MatchEdges(EdgeFilter),
    GetNode(NodeId),
    GetEdge(EdgeId),
    Traverse { kind: TraversalKind, start: NodeId },
    ShortestPath { start: NodeId, goal: NodeId },
    CountNodes,
    CountEdges,
}
```

Both parsers produce `QueryCommand` values. The executor switches on the
variant and calls the appropriate `DatabaseContext` methods.

---

## QueryResult

```rust
pub enum QueryResult {
    Nodes(Vec<Node>),
    Edges(Vec<Edge>),
    Traversal(Vec<NodeId>),
    Path { nodes: Vec<NodeId>, total_weight: f64 },
    SingleNode(Option<Node>),
    SingleEdge(Option<Edge>),
    Count(usize),
    Empty,
}
```

All variants implement `Display` for human-readable output.

---

## SimpleQuery language

**File:** `src/query/languages/simple.rs`

A whitespace-delimited keyword language. Designed for clarity over
expressiveness.

### Grammar (EBNF)

```ebnf
query        = match_cmd | get_cmd | traverse_cmd | path_cmd | count_cmd

match_cmd    = "MATCH" ("NODE" | "EDGE") ["WHERE" filter_chain]
get_cmd      = "GET"   ("NODE" node_ref  | "EDGE"  edge_ref)
traverse_cmd = "TRAVERSE" ("BFS" | "DFS") "FROM" node_ref
path_cmd     = "PATH" "FROM" node_ref "TO" node_ref
count_cmd    = "COUNT" ("NODES" | "EDGES")

filter_chain = condition {"AND" condition}
condition    = "label"     op string_lit
             | "weight"    op number_lit
             | "props." ident op value_lit

op           = "=" | "!=" | "<" | "<=" | ">" | ">="
node_ref     = "N" digits   (* e.g. N0, N42 *)
edge_ref     = "E" digits   (* e.g. E0, E7  *)
string_lit   = '"' chars '"'
number_lit   = integer | float
```

### Full command reference

| Command | Description |
|---------|-------------|
| `MATCH NODE` | All nodes |
| `MATCH NODE WHERE label = "City"` | Nodes with label "City" |
| `MATCH NODE WHERE label = "City" AND props.population > 1000000` | Combined filter |
| `MATCH NODE WHERE props.name = "London"` | Property filter |
| `MATCH EDGE` | All edges |
| `MATCH EDGE WHERE label = "RAIL"` | Edges with label "RAIL" |
| `MATCH EDGE WHERE weight < 500.0` | Edges below weight |
| `MATCH EDGE WHERE label = "RAIL" AND weight < 500` | Combined filter |
| `GET NODE N0` | Single node by ID |
| `GET EDGE E3` | Single edge by ID |
| `TRAVERSE BFS FROM N0` | BFS traversal |
| `TRAVERSE DFS FROM N0` | DFS traversal |
| `PATH FROM N0 TO N4` | Dijkstra shortest path |
| `COUNT NODES` | Total node count |
| `COUNT EDGES` | Total edge count |

### Tokenizer

SimpleQuery tokenizes by splitting on whitespace, with one exception:
quoted strings (`"Big City"`) are kept as a single token even if they
contain spaces. This is handled by a small state machine:

```
in_string = false
for each character:
  if '"': toggle in_string, emit current token
  elif whitespace and not in_string: emit current token
  else: append to current token
```

### Parser

A recursive descent parser. Each command keyword dispatches to a
dedicated method:

```
parse() → match keyword:
  "MATCH"    → parse_match()
  "GET"      → parse_get()
  "TRAVERSE" → parse_traverse()
  "PATH"     → parse_path()
  "COUNT"    → parse_count()
```

---

## CypherLite language

**File:** `src/query/languages/cypher_lite.rs`

A subset of the Cypher graph query language, used by Neo4j.

### Grammar (EBNF)

```ebnf
query        = match_clause | traverse_clause | path_clause | count_clause

match_clause = "MATCH" pattern ["WHERE" where_expr] "RETURN" return_list
pattern      = node_pat ["-[" rel_pat "]->" node_pat]
node_pat     = "(" [var] [":" label] ")"
rel_pat      = [var] [":" label]

where_expr   = condition {"AND" condition}
condition    = var "." prop op value_lit

traverse_clause = "TRAVERSE" ("BFS"|"DFS") "FROM" node_ref
path_clause     = "PATH" "FROM" node_ref "TO" node_ref
count_clause    = "COUNT" ("NODES" | "EDGES")
```

### Full command reference

| Command | Description |
|---------|-------------|
| `MATCH (n) RETURN n` | All nodes |
| `MATCH (n:City) RETURN n` | Nodes with label "City" |
| `MATCH (n:City) WHERE n.population > 1000000 RETURN n` | Label + property |
| `MATCH (n) WHERE n.name = "London" RETURN n` | Property only |
| `MATCH ()-[r]->() RETURN r` | All edges |
| `MATCH ()-[r:RAIL]->() RETURN r` | Edges with label |
| `MATCH ()-[r]->() WHERE r.weight < 500 RETURN r` | Edges by weight |
| `MATCH (a)-[r:RAIL]->(b) RETURN a, r, b` | Full relationship pattern |
| `TRAVERSE BFS FROM N0` | BFS traversal |
| `TRAVERSE DFS FROM N0` | DFS traversal |
| `PATH FROM N0 TO N4` | Dijkstra shortest path |
| `COUNT NODES` | Total node count |
| `COUNT EDGES` | Total edge count |

### Lexer

CypherLite has richer punctuation than SimpleQuery (parentheses, brackets,
`:`, `->`) so it uses a character-level lexer that produces typed tokens:

```rust
enum Token {
    Keyword(String),  // MATCH, WHERE, RETURN, ...
    Ident(String),    // variable names, labels
    NodeRef(u64),     // N0, N42
    StringLit(String),
    IntLit(i64),
    FloatLit(f64),
    BoolLit(bool),
    Eq, NotEq, Lt, LtEq, Gt, GtEq,
    LParen, RParen, LBracket, RBracket,
    Colon, Dot, Comma, Dash, Arrow,  // -  and  ->
}
```

### How the same query looks in both languages

| Intent | SimpleQuery | CypherLite |
|--------|------------|------------|
| All cities | `MATCH NODE WHERE label = "City"` | `MATCH (n:City) RETURN n` |
| Cities > 1M pop | `MATCH NODE WHERE label = "City" AND props.population > 1000000` | `MATCH (n:City) WHERE n.population > 1000000 RETURN n` |
| All rail edges | `MATCH EDGE WHERE label = "RAIL"` | `MATCH ()-[r:RAIL]->() RETURN r` |
| Short legs | `MATCH EDGE WHERE weight < 500` | `MATCH ()-[r]->() WHERE r.weight < 500 RETURN r` |
| BFS from N0 | `TRAVERSE BFS FROM N0` | `TRAVERSE BFS FROM N0` |
| London→Berlin | `PATH FROM N0 TO N2` | `PATH FROM N0 TO N2` |

Both produce identical `QueryCommand` values and identical results.

---

## Adding a third language

### Step 1: Create the file

```rust
// src/query/languages/graphql_lite.rs

pub struct GraphQlLiteLanguage;

impl QueryLanguagePort for GraphQlLiteLanguage {
    fn language_name(&self) -> &str { "GraphQLLite" }

    fn execute(
        &self,
        query: &str,
        context: &mut dyn DatabaseContext,
    ) -> Result<QueryResult, GraphError> {
        let command = parse_graphql(query)?;      // your parser
        executor::execute(command, context)        // shared executor
    }
}

fn parse_graphql(q: &str) -> Result<QueryCommand, GraphError> {
    // Parse: { nodes(label: "City") { id, label, name } }
    // → QueryCommand::MatchNodes(NodeFilter { label: Some("City"), ... })
    todo!()
}
```

### Step 2: Export from the module

```rust
// src/query/languages/mod.rs
pub mod cypher_lite;
pub mod graphql_lite;  // add this
pub mod simple;
```

### Step 3: Use it

```rust
let graphql = GraphQlLiteLanguage;
let result = db.execute_query(&graphql, "{ nodes(label: \"City\") { name } }")?;
```

No other changes. The database, executor, and all other adapters are untouched.

---

## Filter evaluation

Filters are evaluated in `query/ast.rs`. The `NodeFilter::matches` method
checks a node against all conditions:

```rust
impl NodeFilter {
    pub fn matches(&self, node: &Node) -> bool {
        // Label check
        if let Some(ref expected) = self.label {
            if &node.label != expected { return false; }
        }
        // All property conditions must hold
        self.property_conditions
            .iter()
            .all(|cond| cond.matches_properties(&node.properties))
    }
}
```

**When a label filter is present** the executor calls `ctx.get_nodes_by_label(label)`
which performs an O(1) lookup in the in-memory `LabelIndex`, returning only
the candidate set for that label.  Property conditions are then evaluated
against that smaller set — O(label_count × conditions) instead of O(N).

**When no label filter is present** a full scan is still used: `ctx.get_all_nodes()`
loads every node and applies all conditions — O(N).

For range queries on property values (e.g. `population > 1_000_000`) a
BTree-based property index would reduce cost to O(log N + results).
See [10_scale_and_production.md](10_scale_and_production.md).
