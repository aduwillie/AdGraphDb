# 08 — Graph Algorithms

All algorithms implement a port trait and receive the engine as a
`&dyn GraphEnginePort` — no disk access, no property reads, pure computation.

---

## Algorithm port traits

```rust
pub trait TraversalAlgorithm {
    fn traverse(&self, engine: &dyn GraphEnginePort, start: NodeId) -> Vec<NodeId>;
}

pub trait ShortestPathAlgorithm {
    fn find_shortest_path(
        &self,
        engine: &dyn GraphEnginePort,
        start: NodeId,
        goal: NodeId,
    ) -> Option<(Vec<NodeId>, f64)>;
}
```

**Design rationale**: algorithms are zero-sized structs (no state).
They are passed by reference at call time, not stored in the database.
This lets you choose the algorithm per-call:

```rust
db.traverse(&BreadthFirstSearch, start);    // BFS
db.traverse(&DepthFirstSearch, start);      // DFS
db.find_shortest_path(&Dijkstra, a, b);     // Dijkstra
```

---

## BreadthFirstSearch

**File:** `src/algorithms/bfs.rs`

### The intuition

BFS is like throwing a stone into a pond: it visits all nodes one hop away
before visiting nodes two hops away, etc.

```
Graph:  0 → 1 → 3
            ↓
        0 → 2 → 4

BFS from 0:   0   1  2   3  4
              ▔▔▔▔▔   ▔▔▔▔▔   ▔▔▔▔▔
              hop 0   hop 1   hop 2
```

### Algorithm (pseudocode)

```
queue = [start]
visited = {start}
result = []

while queue is not empty:
    node = queue.dequeue()            ← FIFO: take from front
    result.append(node)
    for each neighbor of node:
        if neighbor not in visited:
            visited.add(neighbor)
            queue.enqueue(neighbor)   ← add to back

return result
```

### Rust implementation detail

`std::collections::VecDeque` is Rust's double-ended queue. It gives O(1)
`push_back` (enqueue) and `pop_front` (dequeue):

```rust
let mut queue: VecDeque<NodeId> = VecDeque::new();
queue.push_back(start);          // O(1)
let current = queue.pop_front(); // O(1)
```

### Properties

| Property | Value |
|----------|-------|
| Visit order | Level-by-level (hop distance) |
| Finds shortest hop-path | ✓ (not shortest weighted path — use Dijkstra) |
| Handles cycles | ✓ (HashSet tracks visited) |
| Time complexity | O(V + E) |
| Space complexity | O(V) — queue holds at most V nodes |

### When to use BFS

- Find the shortest hop-count path between two nodes
- Find all nodes within N hops of a starting node
- Level-order traversal (social network "friends of friends")
- Web crawling (explore nearby pages before distant ones)

---

## DepthFirstSearch

**File:** `src/algorithms/dfs.rs`

### The intuition

DFS is like exploring a maze: follow one corridor as far as it goes, then
backtrack and try the next branch.

```
Graph:  0 → 1 → 3
                ↓
        0 → 2 → 4

DFS from 0 (one possible order):  0  2  4  1  3
                                   ▔▔▔▔▔▔▔▔  ▔▔▔▔▔
                                   branch 2   branch 1
```

### Algorithm (iterative, using a stack)

```
stack = [start]
visited = {}
result = []

while stack is not empty:
    node = stack.pop()              ← LIFO: take from top
    if node in visited: continue
    visited.add(node)
    result.append(node)
    for each neighbor of node:
        if neighbor not in visited:
            stack.push(neighbor)

return result
```

### Why iterative instead of recursive?

Recursive DFS calls itself for each node, using the call stack. For a graph
with depth 10,000, this overflows the stack. The iterative version uses an
explicit `Vec` as its stack — no risk of stack overflow.

### Properties

| Property | Value |
|----------|-------|
| Visit order | Branch-by-branch |
| Finds shortest path | ✗ (finds A path, not necessarily shortest) |
| Handles cycles | ✓ (HashSet tracks visited) |
| Time complexity | O(V + E) |
| Space complexity | O(V) — stack holds at most V nodes |

### When to use DFS

- Detect cycles in a graph
- Topological sort of a DAG
- Find all strongly connected components
- Maze solving (any path to the exit)
- Parsing (expression trees are traversed depth-first)

### BFS vs DFS at a glance

| | BFS | DFS |
|--|--|--|
| Data structure | Queue (FIFO, VecDeque) | Stack (LIFO, Vec) |
| Explores | Wide first | Deep first |
| Shortest hop path | ✓ | ✗ |
| Memory for wide graph | High (holds whole level) | Low |
| Memory for deep graph | Low | High |

---

## Dijkstra's Shortest Path

**File:** `src/algorithms/dijkstra.rs`

### The intuition

Dijkstra answers: "What is the cheapest route from city A to city B?"
It's the algorithm used in GPS navigation and network routing.

### The key insight

BFS finds the shortest hop-path. Dijkstra finds the shortest **weighted**
path. Instead of a FIFO queue, it uses a **min-priority queue** (min-heap)
that always gives you the node with the smallest current tentative distance.

### Algorithm

```
distance = {start: 0.0, all others: ∞}
predecessor = {}
heap = min_heap [(0.0, start)]

while heap is not empty:
    (dist, node) = heap.pop_min()

    if node == goal: return reconstruct_path(predecessor, start, goal)

    if dist > distance[node]: continue  ← stale entry, skip

    for each (neighbor, edge_weight) of node:
        new_dist = dist + edge_weight
        if new_dist < distance[neighbor]:
            distance[neighbor] = new_dist
            predecessor[neighbor] = node
            heap.push((new_dist, neighbor))

return None  ← goal unreachable
```

### Stale entry skipping

The heap may contain multiple entries for the same node (from different
relaxation rounds). We skip entries whose stored distance is worse than
the best we've already found:

```
heap: [(3.0, N1), (5.0, N1)]  ← N1 was relaxed twice

pop (3.0, N1) → dist (3.0) == distance[N1] (3.0) → process ✓
pop (5.0, N1) → dist (5.0) > distance[N1] (3.0) → skip   ✓
```

### Min-heap in Rust

`std::collections::BinaryHeap` is a **max**-heap (pops the largest value).
We wrap entries in `Reverse<>` to invert the ordering:

```rust
use std::cmp::Reverse;
let mut heap: BinaryHeap<Reverse<HeapEntry>> = BinaryHeap::new();
heap.push(Reverse(HeapEntry { distance: 0.0, node_id: start }));
let Reverse(entry) = heap.pop().unwrap();
```

### Path reconstruction

During the search we record `predecessor[node] = the_node_we_came_from`.
After reaching the goal, we walk backwards through predecessors and reverse:

```
predecessor: {N3: N1, N1: N0}

Walk backwards from N3:
  N3 → N1 → N0 (= start, stop)

Reverse: [N0, N1, N3]  ← the path
```

### Properties

| Property | Value |
|----------|-------|
| Finds | Minimum-weight path |
| Requires | Non-negative edge weights |
| Time complexity | O((V + E) log V) with a binary heap |
| Space complexity | O(V) |
| Handles negative weights | ✗ (use Bellman-Ford) |

### When to use Dijkstra

- GPS routing (road weights = travel time)
- Network routing (weights = latency or bandwidth cost)
- Dependency resolution (weights = installation cost)
- Any "cheapest path" problem with non-negative costs

---

## Adding a new algorithm

### New traversal (e.g. random walk)

```rust
// src/algorithms/random_walk.rs
pub struct RandomWalk { pub steps: usize }

impl TraversalAlgorithm for RandomWalk {
    fn traverse(&self, engine: &dyn GraphEnginePort, start: NodeId) -> Vec<NodeId> {
        let mut path = vec![start];
        let mut current = start;
        // (use a real RNG in production)
        for _ in 0..self.steps {
            let neighbors = engine.neighbors_outgoing(current);
            if neighbors.is_empty() { break; }
            current = neighbors[0].node_id;
            path.push(current);
        }
        path
    }
}
```

```rust
// Usage:
db.traverse(&RandomWalk { steps: 10 }, start_id);
```

### New shortest-path (e.g. Bellman-Ford)

```rust
pub struct BellmanFord;

impl ShortestPathAlgorithm for BellmanFord {
    fn find_shortest_path(&self, engine: &dyn GraphEnginePort, start: NodeId, goal: NodeId)
        -> Option<(Vec<NodeId>, f64)>
    {
        // Relax all edges V-1 times. O(VE) time.
        // Handles negative weights. Detects negative cycles.
        todo!()
    }
}
```

### Algorithm ideas to implement

| Algorithm | What it computes |
|-----------|-----------------|
| A\* | Dijkstra + heuristic; faster for geographic graphs |
| Bellman-Ford | Shortest path with negative weights |
| Floyd-Warshall | All-pairs shortest paths in O(V³) |
| Topological sort | Ordering of a DAG |
| Kosaraju / Tarjan | Strongly connected components |
| Prim / Kruskal | Minimum spanning tree |
| PageRank | Node importance by link structure |
| Betweenness centrality | Which nodes are most "central"? |
