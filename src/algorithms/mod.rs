// Algorithms — pluggable graph algorithms.
//
// Each algorithm is a zero-sized struct that implements one of the algorithm
// ports from ports/algorithm.rs.  Pass any of them to the database methods
// that accept a &dyn TraversalAlgorithm or &dyn ShortestPathAlgorithm.
//
// Adding a new algorithm:
//   1. Create a file here (e.g. a_star.rs)
//   2. Implement ShortestPathAlgorithm for your struct
//   3. Expose it from this mod
//   4. Pass it to the database — no other changes needed

pub mod bfs;
pub mod dfs;
pub mod dijkstra;
