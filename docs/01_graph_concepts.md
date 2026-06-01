# 01 — Graph Concepts

> **Read this first.** Everything else in these docs assumes you understand
> the vocabulary in this file.

---

## What is a graph?

In mathematics and computer science, a **graph** is a data structure made of
two things:

- **Vertices** (also called *nodes*) — the "things" in your domain
- **Edges** (also called *relationships* or *arcs*) — the connections between things

### Analogy: a road map

A road map is a graph:
- Cities are **nodes**
- Roads between cities are **edges**
- The distance on a road is the **weight** of that edge

```
London ──457km──> Paris
London ──370km──> Brussels
Paris  ──265km──> Brussels
```

Almost anything can be modelled as a graph:
- Social network: people are nodes, friendships are edges
- Web pages: pages are nodes, hyperlinks are edges
- Supply chain: warehouses are nodes, shipping routes are edges
- Dependencies: packages are nodes, "depends on" edges

---

## Directed vs undirected graphs

An **undirected** edge means the connection goes both ways:
```
Alice ── friends with ── Bob
```
If Alice is Bob's friend, Bob is Alice's friend.

A **directed** edge (an *arc*) has a specific source and target:
```
Alice ──follows──> Bob   (Alice follows Bob, not necessarily vice-versa)
```

AdGraphDb stores **directed** edges. You can model undirected graphs by
inserting two directed edges: one each way.

---

## Weighted graphs

A **weight** is a numeric value attached to an edge. It typically represents
cost, distance, time, or probability.

```
London ──457.0km──> Paris       weight = 457.0
Paris  ──265.0km──> Brussels    weight = 265.0
```

Algorithms like Dijkstra use weights to find the minimum-cost path.
Unweighted graphs assign every edge `weight = 1.0`.

---

## Paths and reachability

A **path** is a sequence of nodes connected by edges:
```
London → Brussels → Amsterdam
```

A node B is **reachable** from node A if a path exists from A to B.

The **shortest path** is the path with the minimum total weight.

---

## Degree

The **degree** of a node is how many edges it has:
- **Out-degree**: number of outgoing edges
- **In-degree**: number of incoming edges

A node with out-degree 0 is a **sink** (no outgoing edges).
A node with in-degree 0 is a **source** (no incoming edges).

---

## Cycles

A **cycle** is a path that starts and ends at the same node:
```
A → B → C → A   (cycle of length 3)
```

A graph with no cycles is a **DAG** (Directed Acyclic Graph).
DAGs appear frequently in dependency resolution, scheduling, and data pipelines.

BFS and DFS in AdGraphDb handle cycles correctly by tracking visited nodes.

---

## Property graphs

A **property graph** extends the basic graph model:

- Nodes have a **label** (a type, e.g. "Person" or "City") and a **property bag**
  (arbitrary key-value metadata)
- Edges have a **label** (a relationship type, e.g. "KNOWS" or "RAIL") and
  also carry properties and a weight

AdGraphDb uses the property graph model:

```
Node:
  id         NodeId       globally unique identifier
  label      String       the type of this node ("City", "Person", ...)
  properties HashMap      arbitrary metadata ("name": "London", "pop": 9000000)

Edge:
  id         EdgeId       globally unique identifier
  source     NodeId       the node this edge leaves from
  target     NodeId       the node this edge points to
  label      String       the relationship type ("RAIL", "KNOWS", ...)
  weight     f64          numeric cost / distance / strength
  properties HashMap      arbitrary metadata
```

---

## Key graph algorithms

| Algorithm | Question answered |
|-----------|------------------|
| BFS | What nodes are reachable? In what hop-distance order? |
| DFS | What branches exist? (useful for cycle detection, topological sort) |
| Dijkstra | What is the minimum-cost path from A to B? |
| Bellman-Ford | Same as Dijkstra but handles negative weights |
| Floyd-Warshall | Minimum cost between ALL pairs of nodes |
| PageRank | Which nodes are most "important" by link structure? |

AdGraphDb includes BFS, DFS, and Dijkstra as pluggable adapters.
See [08_algorithms.md](08_algorithms.md) for details.

---

## Graph representations in memory

How a computer stores a graph affects which operations are fast:

| Representation | Space | Add edge | Edge exists? | Neighbors of X |
|----------------|-------|----------|--------------|----------------|
| Adjacency list | O(V+E) | O(1) | O(degree) | O(degree) |
| Adjacency matrix | O(V²) | O(1) | O(1) | O(V) |

AdGraphDb uses an **adjacency list** (`AdjacencyListEngine`), which is
space-efficient for sparse graphs (most real graphs have far fewer edges
than V²). See [07_graph_engine.md](07_graph_engine.md).

---

## Glossary

| Term | Definition |
|------|-----------|
| Graph | A set of vertices connected by edges |
| Node / Vertex | A single entity in the graph |
| Edge / Arc | A connection between two nodes |
| Directed | Edges have a source and a target |
| Weight | A numeric value on an edge |
| Path | A sequence of nodes connected by edges |
| Degree | Number of edges incident to a node |
| Cycle | A path that returns to its starting node |
| DAG | Directed Acyclic Graph — no cycles |
| BFS | Breadth-First Search — level-by-level traversal |
| DFS | Depth-First Search — branch-first traversal |
| Property graph | Graph where nodes and edges carry labels and key-value properties |
| WAL | Write-Ahead Log — append-only durability log |
| LRU | Least-Recently-Used — a cache eviction policy |
