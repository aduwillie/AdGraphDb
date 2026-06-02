# 16 — Query Planner and Optimization

> **What this document covers:**
> How AdGraphDb decides *which index to use* for a query, how the cost model
> works, how to see which plan was chosen, and how to extend the planner.

---

## The three-stage query pipeline

```
Language string
      │
      ▼  (1) Parse
 QueryCommand          ← what the query wants (intent)
      │
      ▼  (2) Plan
 ExecutionPlan         ← how to get it (strategy)
      │
      ▼  (3) Execute
 QueryResult           ← the answer
```

Without a planner, stage 2 is skipped: every query uses the same strategy
regardless of available indexes.  With the planner, stage 2 picks the
cheapest available strategy before any data is touched.

---

## ExecutionPlan variants

Each variant corresponds to one code path in `query/executor.rs`.

| Variant | When chosen | Estimated cost |
|---------|-------------|----------------|
| `PropertyIndexScan` | A filter condition targets an indexed field | O(log N + results) |
| `LabelIndexScan` | Filter has a label AND that label is less common than all nodes | O(label_count) |
| `FullNodeScan` | No index applies | O(N) |
| `FullEdgeScan` | Any edge match | O(E) |
| `NodeLookup` | `GET NODE Nx` | O(1) |
| `EdgeLookup` | `GET EDGE Ex` | O(1) |
| `Traverse` | `TRAVERSE BFS/DFS` | O(reachable V+E) |
| `ShortestPath` | `PATH FROM … TO …` | O((V+E) log V) |
| `CountNodes/Edges` | `COUNT NODES/EDGES` | O(1) |

---

## The cost model

`QueryPlanner::plan()` in `src/query/planner.rs` evaluates three strategies:

### Strategy A — PropertyIndexScan

Chosen when any property condition in the filter targets an indexed field
(one present in `DatabaseStats::indexed_node_fields`).

```
estimated_cost = log₂(N) + N/10
```

`log₂(N)` is the BTree seek.  `N/10` is an assumed 10% selectivity (rough
heuristic — a real planner would use column statistics).

### Strategy B — LabelIndexScan

Chosen when the filter has a label AND `label_count < node_count`.
Cost = `label_counts[label]` — the number of nodes that carry that label.

```
cost = label_counts["City"]    // e.g. 1_000 out of 1_000_000 total nodes
```

### Strategy C — FullNodeScan

Fallback when no index applies.

```
cost = N (total nodes)
```

### Selection rule

```
if PropertyIndexScan_cost ≤ LabelIndexScan_cost AND PropertyIndexScan_cost < N:
    choose PropertyIndexScan
elif LabelIndexScan_cost < N:
    choose LabelIndexScan
else:
    choose FullNodeScan
```

---

## DatabaseStats — the planner's input

`DatabaseContext::stats()` is called at the start of every `execute()` call.
It returns a cheap snapshot (no I/O):

```rust
pub struct DatabaseStats {
    pub node_count:          usize,
    pub edge_count:          usize,
    pub label_counts:        HashMap<String, usize>,  // from LabelIndex
    pub indexed_node_fields: HashSet<String>,          // from PropertyIndex
}
```

`LayeredGraphDatabase::build_stats()` assembles this by iterating the
in-memory `LabelIndex` keys (O(distinct_labels)) and `PropertyIndex` field
names (O(distinct_fields)).  Both are O(1) per distinct value.

---

## EXPLAIN — seeing the chosen plan

```rust
let (result, plan_desc) = db.execute_query_with_explain(
    &SimpleQueryLanguage,
    "MATCH NODE WHERE label = \"City\" AND props.population > 1000000",
)?;

println!("Plan: {plan_desc}");
// Plan: stats: 1000 nodes, 500 labeled, 3 indexed fields  |  elapsed: 42 µs
```

For a full plan description, use `ExecutionPlan::describe(&stats)`:

```rust
use ad_graph_db::query::{
    ast::QueryCommand,
    planner::{DatabaseStats, ExecutionPlan, QueryPlanner},
};

let stats   = db.stats();  // or build one manually in tests
let command = QueryCommand::MatchNodes(filter);
let plan    = QueryPlanner::plan(command, &stats);
println!("{}", plan.describe(&stats));
// PropertyIndexScan(population > Integer(1000000))  est. cost ~110
```

---

## Property index — how it works

`src/adapters/index/property_index.rs` — a `PropertyIndex` per field, each
containing type-separated BTreeMaps:

```
field "population"
  integers:  BTreeMap { 1_200_000 → [N3], 2_100_000 → [N1], 3_700_000 → [N2], 9_000_000 → [N0] }

field "name"
  texts:     BTreeMap { "Berlin" → [N2], "Brussels" → [N3], "London" → [N0], "Paris" → [N1] }
```

Range query `WHERE population > 2_000_000`:

```rust
integers.range((Excluded(2_000_000), Unbounded))
// → iterates BTree entries ≥ 2_000_001: [N1, N2, N0]
// O(log N) seek + O(results) walk
```

Text equality `WHERE name = "London"`:

```rust
texts.get("London") → Some([N0])
// O(log N) BTree lookup
```

**Type separation** avoids the `f64: !Ord` problem.  Floats go into a
`BTreeMap<OrderedF64, Vec<NodeId>>` where `OrderedF64` provides a total
order (NaN sorts below -∞).

---

## Configuration — which fields are indexed

`DatabaseConfig::indexed_node_fields` controls which property fields are
added to the `PropertyIndex`:

```rust
// Index all fields (default — maximises query speed):
DatabaseConfig::default()

// Index only specific fields (less RAM, faster insert):
DatabaseConfig {
    indexed_node_fields: NodeFieldIndexing::OnlyFields(
        vec!["population".into(), "name".into()]
    ),
    ..Default::default()
}

// No property index — all property filters use full scan:
DatabaseConfig {
    indexed_node_fields: NodeFieldIndexing::None,
    ..Default::default()
}
```

---

## Metrics — observing planner effectiveness

```rust
let m = db.metrics();
println!("Label index hits    : {}", m.label_index_hits);
println!("Property index hits : {}", m.property_index_hits);
println!("Full node scans     : {}", m.full_node_scans);
println!("Index hit rate      : {:.1}%", m.index_hit_rate() * 100.0);
println!("{m}");  // full formatted report
```

If `full_node_scans` is high, add property indexes for the most-filtered fields.

---

## Auto-compaction

The WAL grows on every write.  Left unchecked, replay time grows with it.

```rust
DatabaseConfig {
    auto_compact_after_writes: Some(10_000),  // default
    // None = manual compact() only
    ..Default::default()
}
```

After `10_000` write operations, the database automatically compacts the WAL
to contain only live records.  The cache is cleared on compaction (it will
be repopulated lazily).

```rust
db.compact()?;   // also available manually at any time
```

---

## Extending the planner

To add a new strategy (e.g. an edge label index):

```
1. Add a new ExecutionPlan variant:
     ExecutionPlan::EdgeLabelScan { label: String, remaining_filter: EdgeFilter }

2. Add a cost estimate in QueryPlanner::plan_match_edges:
     let label_cost = stats.edge_label_counts.get(label).copied().unwrap_or(E);
     if label_cost < E {
         return ExecutionPlan::EdgeLabelScan { label, remaining_filter: filter };
     }

3. Add dispatch in executor::execute_plan:
     ExecutionPlan::EdgeLabelScan { label, remaining_filter } => {
         let candidates = ctx.get_edges_by_label(&label)?;
         ...
     }

4. Add DatabaseContext::get_edges_by_label() and implement it.
```

No other files need to change.

---

## Summary

| Component | File | Role |
|-----------|------|------|
| `QueryPlanner` | `src/query/planner.rs` | Cost model, strategy selection |
| `ExecutionPlan` | `src/query/planner.rs` | Chosen strategy |
| `DatabaseStats` | `src/query/planner.rs` | Planner's view of the database |
| `executor::execute()` | `src/query/executor.rs` | Plans + executes in one call |
| `executor::execute_plan()` | `src/query/executor.rs` | Runs a pre-computed plan |
| `PropertyIndex` | `src/adapters/index/property_index.rs` | BTree field index |
| `LabelIndex` | `src/adapters/index/label_index.rs` | HashMap label index |
| `DatabaseConfig` | `src/database/config.rs` | Which fields to index |
| `DatabaseMetrics` | `src/database/metrics.rs` | Runtime usage statistics |
